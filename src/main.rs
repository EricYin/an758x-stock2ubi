#[cfg(target_os = "linux")]
mod install;
#[cfg(target_os = "linux")]
mod mtd;
#[cfg(target_os = "linux")]
mod unlock;

#[cfg(target_os = "linux")]
mod linux_app {
    use an758x_stock2ubi::display;
    use axum::body::Body;
    use axum::extract::{DefaultBodyLimit, Multipart, Path};
    use axum::http::{HeaderValue, StatusCode, header};
    use axum::response::{Html, IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde::Serialize;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio_util::io::ReaderStream;

    use crate::{
        install,
        mtd::{self, Partition},
        unlock,
    };

    const INDEX_HTML: &str = include_str!("index.html");
    const MAX_UPLOAD_SIZE: usize = 4 * 1024 * 1024;
    // The bundled module carries this kernel release in its vermagic.
    const STOCK_KERNEL_RELEASE: &str = "5.4.55";
    static FLASH_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

    #[derive(Serialize)]
    struct ApiMessage {
        message: String,
    }

    fn html_escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    fn partition_table(partitions: &[Partition]) -> String {
        if partitions.is_empty() {
            return "<p class=\"hint\" data-t=\"noPartitions\">No MTD partitions found.</p>"
                .to_string();
        }

        let mut html = String::from(
            "<table><thead><tr><th data-t=\"device\">Device</th><th data-t=\"name\">Name</th><th data-t=\"size\">Size</th><th data-t=\"eraseBlock\">Erase block</th><th data-t=\"physicalOffset\">Physical offset</th><th></th></tr></thead><tbody>",
        );
        for partition in partitions {
            let offset = partition
                .offset
                .map(|value| format!("0x{value:08x}"))
                .unwrap_or_else(|| "—".to_string());
            html.push_str(&format!(
                "<tr><td>mtd{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><a data-t=\"download\" href=\"/backup/{}\">Download</a></td></tr>",
                partition.index,
                html_escape(&partition.name),
                display::format_size(partition.size),
                display::format_size(partition.erase_size),
                offset,
                partition.index,
            ));
        }
        html.push_str("</tbody></table>");
        html
    }

    fn no_store(response: &mut Response) {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }

    fn text_response(status: StatusCode, message: impl Into<String>) -> Response {
        let mut response = (status, message.into()).into_response();
        no_store(&mut response);
        response
    }

    fn json_response(status: StatusCode, message: impl Into<String>) -> Response {
        let mut response = Json(ApiMessage {
            message: message.into(),
        })
        .into_response();
        *response.status_mut() = status;
        no_store(&mut response);
        response
    }

    async fn index() -> Response {
        let partitions = mtd::discover_partitions().unwrap_or_default();
        let page = INDEX_HTML
            .replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
            .replace("{{PARTITIONS}}", &partition_table(&partitions));
        let mut response = Html(page).into_response();
        no_store(&mut response);
        response
    }

    fn backup_filename(partition: &Partition) -> String {
        format!("mtd{}-{}.bin", partition.index, partition.name)
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .collect()
    }

    async fn download_backup(Path(index): Path<u32>) -> Response {
        let partitions = match mtd::discover_partitions() {
            Ok(partitions) => partitions,
            Err(error) => {
                return text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Reading the MTD partition table failed: {error}"),
                );
            }
        };
        let Some(partition) = partitions
            .into_iter()
            .find(|partition| partition.index == index)
        else {
            return text_response(StatusCode::NOT_FOUND, "MTD partition not found");
        };

        let file = match tokio::fs::File::open(&partition.path).await {
            Ok(file) => file,
            Err(error) => {
                return text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Opening {} failed: {error}", partition.path.display()),
                );
            }
        };

        // The response length follows /proc/mtd; Tokio streams the raw device.
        let stream = ReaderStream::with_capacity(file.take(partition.size), 64 * 1024);
        let disposition = format!("attachment; filename=\"{}\"", backup_filename(&partition));
        let mut response = Response::new(Body::from_stream(stream));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&partition.size.to_string()).unwrap(),
        );
        response.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&disposition).unwrap(),
        );
        no_store(&mut response);
        response
    }

    fn schedule_reboot() {
        tokio::spawn(async {
            // Allow the success response to reach the browser before reboot.
            tokio::time::sleep(Duration::from_millis(800)).await;
            unsafe {
                libc::sync();
                if libc::reboot(libc::RB_AUTOBOOT) != 0 {
                    eprintln!("Reboot failed: {}", io::Error::last_os_error());
                    FLASH_IN_PROGRESS.store(false, Ordering::Release);
                }
            }
        });
    }

    async fn flash(mut multipart: Multipart) -> Response {
        let mut bl2 = None;
        let mut fip_image = None;

        loop {
            let field = match multipart.next_field().await {
                Ok(Some(field)) => field,
                Ok(None) => break,
                Err(error) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        format!("Parsing the upload failed: {error}"),
                    );
                }
            };
            let Some(name) = field.name().map(str::to_owned) else {
                continue;
            };
            if name != "bl2" && name != "fip" {
                continue;
            }
            let data = match field.bytes().await {
                Ok(data) => data.to_vec(),
                Err(error) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        format!("Reading the uploaded file failed: {error}"),
                    );
                }
            };
            match name.as_str() {
                "bl2" => bl2 = Some(data),
                "fip" => fip_image = Some(data),
                _ => {}
            }
        }

        let Some(bl2) = bl2.filter(|data| !data.is_empty()) else {
            return json_response(StatusCode::BAD_REQUEST, "Select a BL2 file");
        };
        let Some(fip_image) = fip_image.filter(|data| !data.is_empty()) else {
            return json_response(StatusCode::BAD_REQUEST, "Select a BL31 + U-Boot FIP file");
        };
        if FLASH_IN_PROGRESS
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return json_response(StatusCode::CONFLICT, "A flash operation is already running");
        }

        // Flash writes block a worker thread; the HTTP runtime stays responsive.
        let result = tokio::task::spawn_blocking(move || install::flash(&bl2, &fip_image)).await;
        match result {
            Ok(Ok(message)) => {
                schedule_reboot();
                json_response(StatusCode::OK, message)
            }
            Ok(Err(error)) => {
                FLASH_IN_PROGRESS.store(false, Ordering::Release);
                json_response(StatusCode::BAD_REQUEST, error)
            }
            Err(error) => {
                FLASH_IN_PROGRESS.store(false, Ordering::Release);
                json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Flash task exited unexpectedly: {error}"),
                )
            }
        }
    }

    fn parse_arguments() -> Result<(String, bool), String> {
        let mut listen = "0.0.0.0:3333".to_string();
        let mut ignore_kernel_version = false;
        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--listen" => {
                    listen = arguments
                        .next()
                        .ok_or_else(|| "--listen requires an address".to_string())?;
                }
                "--ignore-kernel-version" => ignore_kernel_version = true,
                "--help" | "-h" => {
                    println!(
                        "usage: an758x-stock2ubi [--listen 0.0.0.0:3333] [--ignore-kernel-version]"
                    );
                    std::process::exit(0);
                }
                _ => return Err(format!("Unknown argument: {argument}")),
            }
        }
        Ok((listen, ignore_kernel_version))
    }

    fn check_kernel_version(ignore_kernel_version: bool) -> Result<(), String> {
        if ignore_kernel_version {
            return Ok(());
        }
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map_err(|error| format!("Reading the kernel release failed: {error}"))?;
        let release = release.trim();
        if release != STOCK_KERNEL_RELEASE {
            return Err(format!(
                "Kernel release is {release}; expected {STOCK_KERNEL_RELEASE}. Use --ignore-kernel-version to skip this check"
            ));
        }
        Ok(())
    }

    pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
        let (listen, ignore_kernel_version) = parse_arguments()?;
        check_kernel_version(ignore_kernel_version)?;
        if unsafe { libc::geteuid() } == 0 {
            match unlock::load() {
                Ok(()) => println!("MTD write protection cleared"),
                Err(error) => eprintln!("MTD unlock failed: {error}"),
            }
        } else {
            eprintln!("MTD unlock requires root privileges");
        }
        let app = Router::new()
            .route("/", get(index))
            .route("/backup/{index}", get(download_backup))
            .route("/api/flash", post(flash))
            .layer(DefaultBodyLimit::max(MAX_UPLOAD_SIZE));
        let listener = tokio::net::TcpListener::bind(&listen).await?;
        println!("AN758x Stock2UBI listening on http://{listen}");
        axum::serve(listener, app).await?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux_app::run().await
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("an758x-stock2ubi requires Linux MTD character devices");
}
