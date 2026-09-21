const UBI_EC_HDR_MAGIC: u32 = 0x5542_4923;
const UBI_VID_HDR_MAGIC: u32 = 0x5542_4921;
const UBI_LAYOUT_VOLUME_ID: u32 = 0x7fff_efff;
const UBI_MAX_VOLUMES: usize = 128;
const UBI_VTBL_RECORD_SIZE: usize = 172;
const UBI_EC_HDR_SIZE: usize = 64;
const UBI_VID_HDR_SIZE: usize = 64;
const UBI_VID_DYNAMIC: u8 = 1;
const UBI_VID_STATIC: u8 = 2;
const UBI_LAYOUT_VOLUME_COMPAT: u8 = 5;
const UBI_CRC32_INIT: u32 = 0xffff_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub erase_size: usize,
    pub page_size: usize,
}

impl Geometry {
    pub fn validate(self) -> Result<(), String> {
        if self.page_size == 0 || self.erase_size < self.page_size * 3 {
            return Err("Invalid NAND page or eraseblock geometry".to_string());
        }
        if !self.erase_size.is_multiple_of(self.page_size) {
            return Err("Eraseblock size must be a multiple of page size".to_string());
        }
        if self.page_size < UBI_EC_HDR_SIZE || self.page_size < UBI_VID_HDR_SIZE {
            return Err("NAND page is too small for a UBI header".to_string());
        }
        Ok(())
    }

    pub fn data_offset(self) -> usize {
        self.page_size * 2
    }

    pub fn leb_size(self) -> usize {
        self.erase_size - self.data_offset()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapInfo {
    pub image: Vec<u8>,
    pub fip_pebs: usize,
    pub total_pebs: usize,
}

fn put_u16_be(dst: &mut [u8], offset: usize, value: u16) {
    dst[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn put_u32_be(dst: &mut [u8], offset: usize, value: u32) {
    dst[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn put_u64_be(dst: &mut [u8], offset: usize, value: u64) {
    dst[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
}

/// UBI stores the Linux crc32_le() result with an initial value of all ones.
pub fn crc32_ubi(data: &[u8]) -> u32 {
    let mut crc = UBI_CRC32_INIT;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    crc
}

fn write_ec_header(peb: &mut [u8], geometry: Geometry, image_seq: u32, eof: bool) {
    put_u32_be(peb, 0, UBI_EC_HDR_MAGIC);
    peb[4] = 1;
    if eof {
        peb[5..8].copy_from_slice(b"EOF");
    }
    put_u64_be(peb, 8, 0);
    put_u32_be(peb, 16, geometry.page_size as u32);
    put_u32_be(peb, 20, geometry.data_offset() as u32);
    put_u32_be(peb, 24, image_seq);
    let crc = crc32_ubi(&peb[..60]);
    put_u32_be(peb, 60, crc);
}

#[allow(clippy::too_many_arguments)]
fn write_vid_header(
    peb: &mut [u8],
    geometry: Geometry,
    vol_type: u8,
    compat: u8,
    vol_id: u32,
    lnum: u32,
    data_size: u32,
    used_ebs: u32,
    data_crc: u32,
    sqnum: u64,
) {
    let header = &mut peb[geometry.page_size..geometry.page_size + UBI_VID_HDR_SIZE];
    put_u32_be(header, 0, UBI_VID_HDR_MAGIC);
    header[4] = 1;
    header[5] = vol_type;
    header[6] = 0;
    header[7] = compat;
    put_u32_be(header, 8, vol_id);
    put_u32_be(header, 12, lnum);
    put_u32_be(header, 20, data_size);
    put_u32_be(header, 24, used_ebs);
    put_u32_be(header, 28, 0);
    put_u32_be(header, 32, data_crc);
    put_u64_be(header, 40, sqnum);
    let crc = crc32_ubi(&header[..60]);
    put_u32_be(header, 60, crc);
}

fn volume_table(fip_pebs: usize) -> Vec<u8> {
    let mut table = vec![0u8; UBI_MAX_VOLUMES * UBI_VTBL_RECORD_SIZE];

    for record in table.as_chunks_mut::<UBI_VTBL_RECORD_SIZE>().0 {
        let crc = crc32_ubi(&record[..168]);
        put_u32_be(record, 168, crc);
    }

    let fip = &mut table[..UBI_VTBL_RECORD_SIZE];
    put_u32_be(fip, 0, fip_pebs as u32);
    put_u32_be(fip, 4, 1);
    put_u32_be(fip, 8, 0);
    fip[12] = UBI_VID_STATIC;
    fip[13] = 0;
    put_u16_be(fip, 14, 3);
    fip[16..19].copy_from_slice(b"fip");
    fip[144] = 0;
    let crc = crc32_ubi(&fip[..168]);
    put_u32_be(fip, 168, crc);
    table
}

fn empty_peb(geometry: Geometry) -> Vec<u8> {
    vec![0xff; geometry.erase_size]
}

/// The EOF marker ends this BL2's UBI scan ahead of the stock image.
pub fn build_bootstrap(
    fip: &[u8],
    geometry: Geometry,
    image_seq: u32,
) -> Result<BootstrapInfo, String> {
    geometry.validate()?;
    if fip.is_empty() {
        return Err("FIP volume content is empty".to_string());
    }

    let leb_size = geometry.leb_size();
    let fip_pebs = fip.len().div_ceil(leb_size);
    let total_pebs = 2 + fip_pebs + 1;
    let table = volume_table(fip_pebs);
    if table.len() > leb_size {
        return Err("UBI volume table exceeds one LEB".to_string());
    }

    let mut image = Vec::with_capacity(total_pebs * geometry.erase_size);
    for lnum in 0..2u32 {
        let mut peb = empty_peb(geometry);
        write_ec_header(&mut peb, geometry, image_seq, false);
        write_vid_header(
            &mut peb,
            geometry,
            UBI_VID_DYNAMIC,
            UBI_LAYOUT_VOLUME_COMPAT,
            UBI_LAYOUT_VOLUME_ID,
            lnum,
            0,
            0,
            0,
            u64::from(lnum) + 1,
        );
        let data_offset = geometry.data_offset();
        peb[data_offset..data_offset + table.len()].copy_from_slice(&table);
        image.extend_from_slice(&peb);
    }

    for (lnum, chunk) in fip.chunks(leb_size).enumerate() {
        let mut peb = empty_peb(geometry);
        write_ec_header(&mut peb, geometry, image_seq, false);
        write_vid_header(
            &mut peb,
            geometry,
            UBI_VID_STATIC,
            0,
            0,
            lnum as u32,
            chunk.len() as u32,
            fip_pebs as u32,
            crc32_ubi(chunk),
            lnum as u64 + 3,
        );
        let data_offset = geometry.data_offset();
        peb[data_offset..data_offset + chunk.len()].copy_from_slice(chunk);
        image.extend_from_slice(&peb);
    }

    let mut eof = empty_peb(geometry);
    write_ec_header(&mut eof, geometry, image_seq, true);
    image.extend_from_slice(&eof);

    Ok(BootstrapInfo {
        image,
        fip_pebs,
        total_pebs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_be_u32(data: &[u8], offset: usize) -> u32 {
        u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn crc_matches_stock_ec_header() {
        // Reference CRC from the first EC header of the HG5382A stock apps UBI.
        let mut header = [0u8; 60];
        put_u32_be(&mut header, 0, UBI_EC_HDR_MAGIC);
        header[4] = 1;
        put_u64_be(&mut header, 8, 0);
        put_u32_be(&mut header, 16, 2048);
        put_u32_be(&mut header, 20, 4096);
        put_u32_be(&mut header, 24, 1_020_850_693);
        assert_eq!(crc32_ubi(&header), 0xb515_1e92);
    }

    #[test]
    fn builds_layout_fip_and_eof_pebs() {
        let geometry = Geometry {
            erase_size: 0x20000,
            page_size: 0x800,
        };
        let fip = vec![0x5a; geometry.leb_size() + 17];
        let bootstrap = build_bootstrap(&fip, geometry, 0x1234_5678).unwrap();
        assert_eq!(bootstrap.fip_pebs, 2);
        assert_eq!(bootstrap.total_pebs, 5);
        assert_eq!(bootstrap.image.len(), geometry.erase_size * 5);

        let first = &bootstrap.image[..geometry.erase_size];
        assert_eq!(read_be_u32(first, 0), UBI_EC_HDR_MAGIC);
        assert_eq!(read_be_u32(first, geometry.page_size), UBI_VID_HDR_MAGIC);
        assert_eq!(
            read_be_u32(first, geometry.page_size + 8),
            UBI_LAYOUT_VOLUME_ID
        );

        let eof = &bootstrap.image[geometry.erase_size * 4..];
        assert_eq!(&eof[5..8], b"EOF");
        assert_eq!(read_be_u32(eof, 60), crc32_ubi(&eof[..60]));
    }
}
