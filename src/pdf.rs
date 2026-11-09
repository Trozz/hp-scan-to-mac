//! Builds a PDF that embeds JPEG pages unchanged (DCTDecode), one image per page.

use crate::jpeg::JpegInfo;

pub struct Page {
    pub jpeg: Vec<u8>,
    pub info: JpegInfo,
    pub dpi: u32,
}

pub fn build(pages: &[Page]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let page_ids: Vec<usize> = (0..pages.len()).map(|n| 3 + n * 3).collect();

    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", offsets.len()).as_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    };

    obj(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = page_ids.iter().map(|id| format!("{id} 0 R")).collect();
    obj(
        &mut out,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        )
        .as_bytes(),
    );

    for (page, id) in pages.iter().zip(&page_ids) {
        let dpi = page.dpi.max(1) as f64;
        let w = page.info.width as f64 * 72.0 / dpi;
        let h = page.info.height as f64 * 72.0 / dpi;
        let colorspace = match page.info.components {
            1 => "/DeviceGray",
            4 => "/DeviceCMYK",
            _ => "/DeviceRGB",
        };
        obj(
            &mut out,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w:.2} {h:.2}] \
                 /Resources << /XObject << /Im0 {} 0 R >> >> /Contents {} 0 R >>",
                id + 1,
                id + 2
            )
            .as_bytes(),
        );
        let mut image = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {colorspace} \
             /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            page.info.width,
            page.info.height,
            page.jpeg.len()
        )
        .into_bytes();
        image.extend(&page.jpeg);
        image.extend(b"\nendstream");
        obj(&mut out, &image);
        let content = format!("q {w:.2} 0 0 {h:.2} 0 0 cm /Im0 Do Q");
        obj(
            &mut out,
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            )
            .as_bytes(),
        );
    }

    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for off in &offsets {
        out.extend(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_valid_structure() {
        let page = || Page {
            jpeg: vec![0xFF, 0xD8, 0xFF, 0xD9],
            info: JpegInfo {
                width: 2550,
                height: 3300,
                components: 3,
            },
            dpi: 300,
        };
        let pdf = build(&[page(), page()]);
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.contains("/Count 2"));
        assert!(text.contains("/Kids [3 0 R 6 0 R]"));
        assert!(text.contains("/MediaBox [0 0 612.00 792.00]"));
        assert!(text.ends_with("%%EOF\n"));

        let xref_at: usize = text
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(pdf[xref_at..].starts_with(b"xref"));
        let xref = String::from_utf8_lossy(&pdf[xref_at..]);
        let first_obj_off: usize = xref.lines().nth(3).unwrap()[..10].parse().unwrap();
        assert!(pdf[first_obj_off..].starts_with(b"1 0 obj"));
    }
}
