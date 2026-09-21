use std::ffi::CString;
use std::io;

const MODULE: &[u8] = include_bytes!("../an758x_mtd_rw.ko");

fn symbol_address(symbols: &str, wanted: &str) -> Result<u64, String> {
    let address = symbols
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let address = fields.next()?;
            fields.next()?;
            (fields.next()? == wanted).then_some(address)
        })
        .next()
        .ok_or_else(|| format!("{wanted} is absent from /proc/kallsyms"))?;
    let address =
        u64::from_str_radix(address, 16).map_err(|_| format!("Invalid address for {wanted}"))?;
    if address == 0 {
        return Err(format!("Address of {wanted} is hidden by the kernel"));
    }
    Ok(address)
}

pub fn load() -> Result<(), String> {
    let symbols = std::fs::read_to_string("/proc/kallsyms")
        .map_err(|error| format!("Reading /proc/kallsyms failed: {error}"))?;
    let get = symbol_address(&symbols, "get_mtd_device")?;
    let put = symbol_address(&symbols, "put_mtd_device")?;

    // The module receives the MTD function addresses from the running kernel.
    let parameters = CString::new(format!(
        "get_mtd_device_addr=0x{get:x} put_mtd_device_addr=0x{put:x}"
    ))
    .unwrap();
    let result = unsafe {
        libc::syscall(
            libc::SYS_init_module,
            MODULE.as_ptr(),
            MODULE.len(),
            parameters.as_ptr(),
        )
    };
    if result == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EEXIST) {
        return Ok(());
    }
    Err(format!("init_module failed: {error}"))
}
