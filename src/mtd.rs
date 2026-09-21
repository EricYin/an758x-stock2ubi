use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Partition {
    pub index: u32,
    pub name: String,
    pub size: u64,
    pub erase_size: u64,
    pub offset: Option<u64>,
    pub path: PathBuf,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct MtdInfoUser {
    type_: u8,
    _padding: [u8; 3],
    flags: u32,
    size: u32,
    erase_size: u32,
    write_size: u32,
    oob_size: u32,
    _padding2: u64,
}

#[derive(Debug)]
pub struct MtdDevice {
    pub partition: Partition,
    raw: File,
    block: File,
    pub write_size: u64,
    pub erase_size: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct BlockTarget {
    pub device_index: usize,
    pub relative_offset: u64,
    pub physical_offset: u64,
}

const IOC_NRBITS: u64 = 8;
const IOC_TYPEBITS: u64 = 8;
const IOC_SIZEBITS: u64 = 14;
const IOC_NRSHIFT: u64 = 0;
const IOC_TYPESHIFT: u64 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u64 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u64 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_WRITE: u64 = 1;
const IOC_READ: u64 = 2;

fn ioctl_number(direction: u64, command: u8, size: usize) -> libc::Ioctl {
    ((direction << IOC_DIRSHIFT)
        | ((b'M' as u64) << IOC_TYPESHIFT)
        | ((command as u64) << IOC_NRSHIFT)
        | ((size as u64) << IOC_SIZESHIFT)) as libc::Ioctl
}

fn parse_number(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else {
        text.parse().ok()
    }
}

fn read_partition_offset(index: u32) -> Option<u64> {
    let path = format!("/sys/class/mtd/mtd{index}/offset");
    fs::read_to_string(path)
        .ok()
        .and_then(|value| parse_number(&value))
        .or_else(|| (index == 0).then_some(0))
}

/// Combine `/proc/mtd` geometry with sysfs offsets on a shared NAND address axis.
pub fn discover_partitions() -> io::Result<Vec<Partition>> {
    let proc_mtd = fs::read_to_string("/proc/mtd")?;
    let mut partitions = Vec::new();

    for line in proc_mtd.lines().skip(1) {
        let Some((device, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(index) = device.strip_prefix("mtd").and_then(|v| v.parse().ok()) else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let Some(size) = fields.next().and_then(|v| u64::from_str_radix(v, 16).ok()) else {
            continue;
        };
        let Some(erase_size) = fields.next().and_then(|v| u64::from_str_radix(v, 16).ok()) else {
            continue;
        };
        let name = fields
            .collect::<Vec<_>>()
            .join(" ")
            .trim_matches('"')
            .to_string();
        let path = PathBuf::from(format!("/dev/mtd{index}"));
        partitions.push(Partition {
            index,
            name,
            size,
            erase_size,
            offset: read_partition_offset(index),
            path,
        });
    }

    partitions.sort_by_key(|partition| partition.index);
    Ok(partitions)
}

impl MtdDevice {
    pub fn open(partition: Partition) -> io::Result<Self> {
        let raw = File::open(&partition.path)?;
        let block = OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/dev/mtdblock{}", partition.index))?;
        let mut info = MtdInfoUser::default();
        let request = ioctl_number(IOC_READ, 1, std::mem::size_of::<MtdInfoUser>());
        let result = unsafe { libc::ioctl(raw.as_raw_fd(), request, &mut info) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if info.erase_size == 0 || info.write_size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MTD reported invalid page or eraseblock geometry",
            ));
        }
        Ok(Self {
            partition,
            raw,
            block,
            write_size: u64::from(info.write_size),
            erase_size: u64::from(info.erase_size),
        })
    }

    pub fn write_target(&self) -> String {
        format!("/dev/mtdblock{}", self.partition.index)
    }

    pub fn read_exact_at(&self, offset: u64, data: &mut [u8]) -> io::Result<()> {
        let mut done = 0;
        while done < data.len() {
            let count = self.raw.read_at(&mut data[done..], offset + done as u64)?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "MTD read ended early",
                ));
            }
            done += count;
        }
        Ok(())
    }

    fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        let mut done = 0;
        while done < data.len() {
            let count = self.block.write_at(&data[done..], offset + done as u64)?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "MTD write returned zero bytes",
                ));
            }
            done += count;
        }
        Ok(())
    }

    pub fn is_bad(&self, offset: u64) -> io::Result<bool> {
        let mut offset = offset as libc::loff_t;
        let request = ioctl_number(IOC_WRITE, 11, std::mem::size_of::<libc::loff_t>());
        let result = unsafe { libc::ioctl(self.raw.as_raw_fd(), request, &mut offset) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result == 1)
        }
    }

    pub fn write_eraseblock(&self, offset: u64, expected: &[u8]) -> io::Result<()> {
        if expected.len() as u64 != self.erase_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Write length does not match the eraseblock size",
            ));
        }
        self.write_all_at(offset, expected).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("mtdblock write at offset 0x{offset:x} failed: {error}"),
            )
        })?;
        // The block device caches one eraseblock; fsync commits it before raw readback.
        self.block.sync_all().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("mtdblock flush at offset 0x{offset:x} failed: {error}"),
            )
        })?;

        let mut actual = vec![0u8; expected.len()];
        self.read_exact_at(offset, &mut actual).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Readback at partition offset 0x{offset:x} failed: {error}"),
            )
        })?;
        if actual != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MTD readback differs from the written data",
            ));
        }
        Ok(())
    }
}

pub fn open_writable_devices(partitions: &[Partition]) -> Vec<MtdDevice> {
    partitions
        .iter()
        .filter(|partition| partition.offset.is_some())
        .filter_map(|partition| MtdDevice::open(partition.clone()).ok())
        .collect()
}

pub fn find_boot_partition(partitions: &[Partition]) -> Option<&Partition> {
    partitions
        .iter()
        .filter(|partition| partition.offset == Some(0) && partition.size >= partition.erase_size)
        .min_by_key(|partition| partition.index)
}

/// Prefer the lowest-index partition when multiple devices cover the first PEB.
pub fn find_boot_device(devices: &[MtdDevice]) -> Option<usize> {
    devices
        .iter()
        .enumerate()
        .filter(|(_, device)| {
            device.partition.offset == Some(0) && device.partition.size >= device.erase_size
        })
        .min_by_key(|(_, device)| device.partition.index)
        .map(|(index, _)| index)
}

fn covering_device(devices: &[MtdDevice], physical_offset: u64, erase_size: u64) -> Option<usize> {
    devices
        .iter()
        .enumerate()
        .filter(|(_, device)| {
            let Some(start) = device.partition.offset else {
                return false;
            };
            let relative = physical_offset.saturating_sub(start);
            start <= physical_offset
                && relative.is_multiple_of(erase_size)
                && relative + erase_size <= device.partition.size
                && device.erase_size == erase_size
        })
        .max_by_key(|(_, device)| device.partition.size)
        .map(|(index, _)| index)
}

/// BL2 scans forward from 0x20000, so the first continuous good run wins.
pub fn find_earliest_good_run(
    devices: &[MtdDevice],
    required_blocks: usize,
    erase_size: u64,
) -> io::Result<Vec<BlockTarget>> {
    if required_blocks == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Required PEB count is zero",
        ));
    }
    let max_end = devices
        .iter()
        .filter_map(|device| {
            device
                .partition
                .offset
                .map(|start| start + device.partition.size)
        })
        .max()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No writable physical MTD range"))?;

    let mut run = Vec::new();
    let mut physical = erase_size;
    while physical + erase_size <= max_end {
        let Some(device_index) = covering_device(devices, physical, erase_size) else {
            run.clear();
            physical += erase_size;
            continue;
        };
        let device = &devices[device_index];
        let relative = physical - device.partition.offset.unwrap();
        match device.is_bad(relative) {
            Ok(false) => run.push(BlockTarget {
                device_index,
                relative_offset: relative,
                physical_offset: physical,
            }),
            Ok(true) | Err(_) => run.clear(),
        }
        if run.len() == required_blocks {
            return Ok(run);
        }
        physical += erase_size;
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("No writable range with {required_blocks} consecutive good blocks"),
    ))
}
