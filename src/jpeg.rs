//! Minimal JPEG header inspection.
//!
//! HP scanners stream ADF pages before they know the page length, so the
//! SOF header carries a height of 0 and the real height is written later in a
//! DNL (Define Number of Lines) marker. Many readers, including PDF viewers,
//! ignore DNL, so we patch the height back into the SOF header.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegInfo {
    pub width: u16,
    pub height: u16,
    pub components: u8,
}

fn be16(buf: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*buf.get(i)?, *buf.get(i + 1)?]))
}

fn is_sof(marker: u8) -> bool {
    matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC)
}

/// Offset of the SOF segment's marker and the line count from a DNL marker, if any.
fn scan(buf: &[u8]) -> Option<(usize, Option<u16>)> {
    if buf.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut sof = None;
    let mut dnl = None;
    let mut i = 2;
    while i + 1 < buf.len() {
        if buf[i] != 0xFF {
            return None;
        }
        let marker = buf[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0xD9 {
            break;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            i += 2;
            continue;
        }
        let len = be16(buf, i + 2)? as usize;
        if is_sof(marker) && sof.is_none() {
            sof = Some(i);
        }
        if marker == 0xDC {
            dnl = be16(buf, i + 4);
        }
        i += 2 + len;
        if marker == 0xDA {
            while i + 1 < buf.len() {
                let next = buf[i + 1];
                if buf[i] == 0xFF && next != 0x00 && !matches!(next, 0xD0..=0xD7) {
                    break;
                }
                i += 1;
            }
        }
    }
    Some((sof?, dnl))
}

fn read_sof(buf: &[u8], sof: usize) -> Option<JpegInfo> {
    Some(JpegInfo {
        height: be16(buf, sof + 5)?,
        width: be16(buf, sof + 7)?,
        components: *buf.get(sof + 9)?,
    })
}

/// Reads dimensions and, when the SOF height is 0, patches it from the DNL marker.
pub fn inspect_and_fix(buf: &mut [u8]) -> Option<JpegInfo> {
    let (sof, dnl) = scan(buf)?;
    let info = read_sof(buf, sof)?;
    if info.height != 0 {
        return Some(info);
    }
    let lines = dnl.filter(|&l| l > 0)?;
    buf[sof + 5..sof + 7].copy_from_slice(&lines.to_be_bytes());
    read_sof(buf, sof)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(height: u16, dnl: Option<u16>) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        v.extend([0xFF, 0xE0, 0x00, 0x10]);
        v.extend(b"JFIF\0");
        v.extend([1, 1, 0, 0, 1, 0, 1, 0, 0]);
        v.extend([0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend(height.to_be_bytes());
        v.extend(640u16.to_be_bytes());
        v.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        v.extend([
            0xFF, 0xDA, 0x00, 0x0C, 3, 1, 0, 2, 0x11, 3, 0x11, 0, 0x3F, 0,
        ]);
        v.extend([0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56]);
        if let Some(lines) = dnl {
            v.extend([0xFF, 0xDC, 0x00, 0x04]);
            v.extend(lines.to_be_bytes());
        }
        v.extend([0xFF, 0xD9]);
        v
    }

    #[test]
    fn reads_normal_header() {
        let mut buf = jpeg(480, None);
        let info = inspect_and_fix(&mut buf).unwrap();
        assert_eq!(
            info,
            JpegInfo {
                width: 640,
                height: 480,
                components: 3
            }
        );
    }

    #[test]
    fn patches_height_from_dnl() {
        let mut buf = jpeg(0, Some(3300));
        let info = inspect_and_fix(&mut buf).unwrap();
        assert_eq!(info.height, 3300);
        assert_eq!(inspect_and_fix(&mut buf).unwrap().height, 3300);
    }

    #[test]
    fn zero_height_without_dnl_is_rejected() {
        let mut buf = jpeg(0, None);
        assert!(inspect_and_fix(&mut buf).is_none());
    }

    #[test]
    fn rejects_non_jpeg() {
        assert!(inspect_and_fix(&mut b"%PDF-1.4".to_vec()).is_none());
    }
}
