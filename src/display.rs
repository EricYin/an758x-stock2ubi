//! Binary size formatting for the partition table.

/// Show fractional units for stock partitions split into header and data MTDs.
pub fn format_size(size: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;

    if size >= MIB {
        if size.is_multiple_of(MIB) {
            format!("{} MiB", size / MIB)
        } else {
            format!("{:.2} MiB", size as f64 / MIB as f64)
        }
    } else if size >= KIB {
        if size.is_multiple_of(KIB) {
            format!("{} KiB", size / KIB)
        } else {
            format!("{:.2} KiB", size as f64 / KIB as f64)
        }
    } else {
        format!("{size} B")
    }
}

#[cfg(test)]
mod tests {
    use super::format_size;

    #[test]
    fn formats_exact_binary_units() {
        assert_eq!(format_size(128 * 1024), "128 KiB");
        assert_eq!(format_size(60 * 1024 * 1024), "60 MiB");
    }

    #[test]
    fn formats_split_rootfs_lengths_in_the_expected_unit() {
        assert_eq!(format_size(8_396), "8.20 KiB");
        assert_eq!(format_size(62_906_164), "59.99 MiB");
        assert_eq!(format_size(4_541_282), "4.33 MiB");
    }
}
