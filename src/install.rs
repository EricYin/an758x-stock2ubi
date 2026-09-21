use an758x_stock2ubi::{fip, ubi};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::mtd;

pub fn flash(bl2: &[u8], fip_image: &[u8]) -> Result<String, String> {
    // Keep the HTTP runtime resident while the installed rootfs is overwritten.
    unsafe {
        libc::mlockall(libc::MCL_CURRENT);
    }

    fip::validate_bl31_uboot(fip_image)?;
    if unsafe { libc::geteuid() } != 0 {
        return Err("Flashing requires root privileges".to_string());
    }

    let partitions = mtd::discover_partitions().map_err(|error| error.to_string())?;
    let devices = mtd::open_writable_devices(&partitions);
    let boot_index = mtd::find_boot_device(&devices).ok_or_else(|| {
        let Some(partition) = mtd::find_boot_partition(&partitions) else {
            return "No MTD partition covers physical offset 0".to_string();
        };
        let error = mtd::MtdDevice::open(partition.clone()).unwrap_err();
        format!("Opening /dev/mtdblock{} failed: {error}", partition.index)
    })?;
    let boot = &devices[boot_index];
    let erase_size =
        usize::try_from(boot.erase_size).map_err(|_| "Eraseblock size is out of range")?;
    let page_size = usize::try_from(boot.write_size).map_err(|_| "Page size is out of range")?;

    let mut current_first_block = vec![0u8; erase_size];
    boot.read_exact_at(0, &mut current_first_block)
        .map_err(|error| format!("Reading the current first block failed: {error}"))?;
    let first_block = fip::prepare_first_block(bl2, &current_first_block, erase_size)?;

    let image_seq = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;
    let bootstrap = ubi::build_bootstrap(
        fip_image,
        ubi::Geometry {
            erase_size,
            page_size,
        },
        image_seq.max(1),
    )?;
    let targets = mtd::find_earliest_good_run(&devices, bootstrap.total_pebs, boot.erase_size)
        .map_err(|error| error.to_string())?;

    boot.write_eraseblock(0, &first_block).map_err(|error| {
        format!(
            "Writing BL2 first block to {} failed: {error}",
            boot.write_target()
        )
    })?;

    for (block, expected) in targets.iter().zip(bootstrap.image.chunks_exact(erase_size)) {
        devices[block.device_index]
            .write_eraseblock(block.relative_offset, expected)
            .map_err(|error| {
                format!(
                    "Writing bootstrap UBI at physical offset 0x{:x} failed: {error}",
                    block.physical_offset
                )
            })?;
    }

    unsafe { libc::sync() };
    let start = targets.first().unwrap().physical_offset;
    let end = targets.last().unwrap().physical_offset + boot.erase_size;
    Ok(format!(
        "BL2 written to {}; bootstrap UBI at 0x{start:x}..0x{end:x}; readback verified; rebooting",
        boot.write_target()
    ))
}
