const TOC_HEADER_NAME: u32 = 0xaa64_0001;
const TOC_HEADER_SIZE: usize = 16;
const TOC_ENTRY_SIZE: usize = 40;

const UUID_TB_FW: [u8; 16] = [
    0x5f, 0xf9, 0xec, 0x0b, 0x4d, 0x22, 0x3e, 0x4d, 0xa5, 0x44, 0xc3, 0x9d, 0x81, 0xc7, 0x3f, 0x0a,
];

const UUID_BL31: [u8; 16] = [
    0x47, 0xd4, 0x08, 0x6d, 0x4c, 0xfe, 0x98, 0x46, 0x9b, 0x95, 0x29, 0x50, 0xcb, 0xbd, 0x5a, 0x00,
];

const UUID_BL33: [u8; 16] = [
    0xd6, 0xd0, 0xee, 0xa7, 0xfc, 0xea, 0xd5, 0x4b, 0x97, 0x82, 0x99, 0x34, 0xf2, 0x34, 0xb6, 0xe4,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FipEntry {
    pub uuid: [u8; 16],
    pub offset: usize,
    pub size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FipInfo {
    pub entries: Vec<FipEntry>,
}

fn read_u32_le(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| "FIP header is truncated".to_string())?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u64_le(data: &[u8], offset: usize) -> Result<u64, String> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or_else(|| "FIP directory entry is truncated".to_string())?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

pub fn parse(data: &[u8]) -> Result<FipInfo, String> {
    if data.len() < TOC_HEADER_SIZE + TOC_ENTRY_SIZE {
        return Err("File is smaller than the minimum FIP directory".to_string());
    }
    if read_u32_le(data, 0)? != TOC_HEADER_NAME {
        return Err("Missing FIP ToC magic 0xaa640001".to_string());
    }

    let mut entries = Vec::new();
    let mut cursor = TOC_HEADER_SIZE;
    let table_end;
    loop {
        let entry = data
            .get(cursor..cursor + TOC_ENTRY_SIZE)
            .ok_or_else(|| "FIP directory terminator is missing".to_string())?;
        let uuid: [u8; 16] = entry[0..16].try_into().unwrap();
        let offset = read_u64_le(entry, 16)?;
        let size = read_u64_le(entry, 24)?;

        if uuid.iter().all(|byte| *byte == 0) {
            // fiptool records the container end here. A padded firstblock can
            // continue with 0xff bytes beyond that boundary.
            let fip_end =
                usize::try_from(offset).map_err(|_| "FIP end offset exceeds platform range")?;
            if size != 0
                || (offset != 0 && (fip_end < cursor + TOC_ENTRY_SIZE || fip_end > data.len()))
                || (offset != 0 && data[fip_end..].iter().any(|byte| *byte != 0xff))
            {
                return Err("FIP terminator boundary does not match file length".to_string());
            }
            table_end = cursor + TOC_ENTRY_SIZE;
            break;
        }

        let offset =
            usize::try_from(offset).map_err(|_| "FIP payload offset exceeds platform range")?;
        let size =
            usize::try_from(size).map_err(|_| "FIP payload length exceeds platform range")?;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| "FIP payload range overflows".to_string())?;
        if size == 0 || end > data.len() {
            return Err("FIP payload extends beyond the file".to_string());
        }
        entries.push(FipEntry { uuid, offset, size });
        cursor += TOC_ENTRY_SIZE;
    }

    if entries.is_empty() {
        return Err("FIP has no payloads".to_string());
    }
    if entries.iter().any(|entry| entry.offset < table_end) {
        return Err("FIP payload overlaps the directory".to_string());
    }

    Ok(FipInfo { entries })
}

fn contains_uuid(info: &FipInfo, expected: &[u8; 16]) -> bool {
    info.entries.iter().any(|entry| &entry.uuid == expected)
}

pub fn validate_preloader(data: &[u8]) -> Result<FipInfo, String> {
    let info = parse(data)?;
    if !contains_uuid(&info, &UUID_TB_FW) {
        return Err("Preloader FIP is missing the TB_FW/BL2 payload".to_string());
    }
    Ok(info)
}

pub fn validate_bl31_uboot(data: &[u8]) -> Result<FipInfo, String> {
    let info = parse(data)?;
    if !contains_uuid(&info, &UUID_BL31) {
        return Err("Boot FIP is missing the BL31 payload".to_string());
    }
    if !contains_uuid(&info, &UUID_BL33) {
        return Err("Boot FIP is missing the BL33/U-Boot payload".to_string());
    }
    Ok(info)
}

/// The plain preloader starts at 0x800; the device's first 0x800 bytes carry
/// the optional BL1 region and are retained when preparing that upload.
pub fn prepare_first_block(
    upload: &[u8],
    current_block: &[u8],
    erase_size: usize,
) -> Result<Vec<u8>, String> {
    const FIP_OFFSET: usize = 0x800;

    if current_block.len() != erase_size || erase_size <= FIP_OFFSET {
        return Err("Current first-block length does not match the NAND eraseblock".to_string());
    }

    if upload.len() == erase_size {
        validate_preloader(&upload[FIP_OFFSET..])?;
        return Ok(upload.to_vec());
    }

    validate_preloader(upload)?;
    if upload.len() > erase_size - FIP_OFFSET {
        return Err("Preloader FIP exceeds the first eraseblock".to_string());
    }

    let mut block = vec![0xff; erase_size];
    block[..FIP_OFFSET].copy_from_slice(&current_block[..FIP_OFFSET]);
    block[FIP_OFFSET..FIP_OFFSET + upload.len()].copy_from_slice(upload);
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_fip(uuids: &[[u8; 16]]) -> Vec<u8> {
        let table_size = TOC_HEADER_SIZE + TOC_ENTRY_SIZE * (uuids.len() + 1);
        let payload_offset = table_size.next_multiple_of(0x400);
        let mut data = vec![0u8; payload_offset + uuids.len() * 16];
        data[0..4].copy_from_slice(&TOC_HEADER_NAME.to_le_bytes());
        data[4..8].copy_from_slice(&0x1234_5678u32.to_le_bytes());

        for (index, uuid) in uuids.iter().enumerate() {
            let cursor = TOC_HEADER_SIZE + index * TOC_ENTRY_SIZE;
            data[cursor..cursor + 16].copy_from_slice(uuid);
            data[cursor + 16..cursor + 24]
                .copy_from_slice(&((payload_offset + index * 16) as u64).to_le_bytes());
            data[cursor + 24..cursor + 32].copy_from_slice(&16u64.to_le_bytes());
        }
        let terminator = TOC_HEADER_SIZE + uuids.len() * TOC_ENTRY_SIZE;
        let fip_end = data.len() as u64;
        data[terminator + 16..terminator + 24].copy_from_slice(&fip_end.to_le_bytes());
        data
    }

    #[test]
    fn validates_required_fip_payloads() {
        validate_preloader(&synthetic_fip(&[UUID_TB_FW])).unwrap();
        validate_bl31_uboot(&synthetic_fip(&[UUID_BL31, UUID_BL33])).unwrap();
        assert!(validate_bl31_uboot(&synthetic_fip(&[UUID_BL31])).is_err());
    }

    #[test]
    fn preserves_bl1_for_plain_preloader() {
        let preloader = synthetic_fip(&[UUID_TB_FW]);
        let mut current = vec![0xff; 0x20000];
        current[..0x800].fill(0x5a);
        let block = prepare_first_block(&preloader, &current, 0x20000).unwrap();
        assert_eq!(&block[..0x800], &current[..0x800]);
        assert_eq!(&block[0x800..0x800 + preloader.len()], &preloader);
    }

    #[test]
    fn accepts_preloader_embedded_in_firstblock() {
        let preloader = synthetic_fip(&[UUID_TB_FW]);
        let mut firstblock = vec![0xff; 0x20000];
        firstblock[..0x800].fill(0x3c);
        firstblock[0x800..0x800 + preloader.len()].copy_from_slice(&preloader);
        let current = vec![0xff; 0x20000];
        let prepared = prepare_first_block(&firstblock, &current, 0x20000).unwrap();
        assert_eq!(prepared, firstblock);
    }
}
