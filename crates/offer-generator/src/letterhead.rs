//! A company's own templates, derived from Aust's (docs/MULTI_TENANT.md).
//!
//! Aust's templates carry its letterhead in a fixed set of places: company name,
//! street and town, phone, mail, website, AGB link and slogan in cells and text
//! boxes; name, bank, IBAN/BIC, tax number and VAT id in the page footer; the logo
//! as the template's images. [`derive`] swaps exactly those for another company's
//! [`Letterhead`] and keeps everything else — layout, columns, formulas, terms —
//! so a new company has working offers, invoices and travel-expense forms on day
//! one. Without a logo the image area stays empty, never Aust's.

use std::io::{Cursor, Read, Write};

use image::{DynamicImage, GenericImageView, ImageFormat, RgbaImage};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::zip_util::{map_io, map_zip};
use crate::OfferError;

/// What a company's documents say about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Letterhead {
    /// Full company name (`tenants.name`).
    pub name: String,
    /// Short name, as in the slogan "Ihr Umzug in besten Händen bei …!".
    pub short_name: String,
    /// Owner, written into the file's author metadata.
    pub owner_name: String,
    pub street: String,
    pub postal_code: String,
    pub city: String,
    pub phone: String,
    pub email: String,
    pub website: String,
    /// Where the terms are published, printed on page 2 of the offer.
    pub agb_url: String,
    pub bank_name: String,
    pub iban: String,
    pub bic: String,
    /// Steuernummer.
    pub tax_number: String,
    /// USt-IdNr.
    pub vat_id: String,
    /// Logo (PNG, JPEG or WebP). `None` leaves the logo area empty.
    pub logo: Option<Vec<u8>>,
    /// Accent colour `#rrggbb`: replaces Aust's orange in table headers. Empty
    /// keeps the orange.
    pub accent: String,
}

impl Letterhead {
    fn town(&self) -> String {
        format!("{} {}", self.postal_code, self.city).trim().to_string()
    }

    /// Aust's literals and what replaces them, longest first so a longer literal
    /// is never half-replaced by a shorter one. Values are XML-escaped.
    fn replacements(&self) -> Vec<(&'static str, String)> {
        let x = xml_escape;
        vec![
            ("Aust Umzüge &amp; Haushaltssauflösungen", x(&self.name)),
            ("Aust Umzüge &amp; Haushaltsauflösungen", x(&self.name)),
            ("Aust Umzüge und Haushaltsauflösungen", x(&self.name)),
            ("Aust Umzüge Haushaltsauflösungen", x(&self.name)),
            ("Ehrlicherstr. 38  31135 Hildesheim", x(&format!("{}  {}", self.street, self.town()))),
            ("www.aust-umzuege.de/rechtliches/agbs", x(&self.agb_url)),
            ("Ihr Umzug in besten Händen bei Aust!", x(&format!("Ihr Umzug in besten Händen bei {}!", self.short_name))),
            ("Ehrlicherstraße 38", x(&self.street)),
            ("Ehrlicherstr. 38", x(&self.street)),
            ("Kaiserstr. 32", x(&self.street)),
            ("31134  Hildesheim", x(&self.town())),
            ("31135 Hildesheim", x(&self.town())),
            ("info@aust-umzuege.de", x(&self.email)),
            ("www.aust-umzuege.de", x(&self.website)),
            ("05121 / 7558379", x(&self.phone)),
            ("Ekaterina.Aust", x(&self.owner_name)),
            ("Alex Aust", x(&self.owner_name)),
            ("<Company>Aust</Company>", format!("<Company>{}</Company>", x(&self.short_name))),
            ("<t>Aust</t>", format!("<t>{}</t>", x(&self.short_name))),
        ]
    }

    /// The page footer in Excel's header/footer codes: name and address left, bank
    /// centre, tax numbers right — the same shape as Aust's, so the invoice's
    /// cash-receipt rewrite of the centre section keeps working.
    fn footer(&self) -> String {
        // In header/footer text a literal `&` is written `&&`.
        let f = |s: &str| s.replace('&', "&&");
        let mut right = vec![];
        if !self.tax_number.is_empty() {
            right.push(format!(" St.Nr.: {} ", f(&self.tax_number)));
        }
        if !self.vat_id.is_empty() {
            right.push(format!("USt-IdNr.: {}", f(&self.vat_id)));
        }
        let code = format!(
            "&L&8{}\n{}\n{} &C&8{}\nIBAN: {}\nBIC: {}&R&8{}",
            f(&self.name),
            f(&self.street),
            f(&self.town()),
            f(&self.bank_name),
            f(&self.iban),
            f(&self.bic),
            right.join("\n"),
        );
        xml_escape(&code)
    }
}

/// `#rrggbb` → Excel's `FFRRGGBB`.
fn accent_argb(accent: &str) -> Option<String> {
    let hex = accent.strip_prefix('#')?;
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("FF{}", hex.to_uppercase()))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Replace the content of every `<oddFooter>…</oddFooter>`.
fn replace_footers(xml: &str, footer: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<oddFooter>") {
        let open_end = start + "<oddFooter>".len();
        let Some(close) = rest[open_end..].find("</oddFooter>") else { break };
        out.push_str(&rest[..open_end]);
        out.push_str(footer);
        rest = &rest[open_end + close..];
    }
    out.push_str(rest);
    out
}

/// `logo` fitted into a canvas of `width`×`height` (aspect kept, centred), or an
/// empty canvas. Encoded like the image it replaces (JPEG on white, else PNG).
fn fit_logo(logo: Option<&[u8]>, width: u32, height: u32, format: ImageFormat) -> Result<Vec<u8>, OfferError> {
    let mut canvas = RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 0]));
    if let Some(bytes) = logo {
        let img = image::load_from_memory(bytes)
            .map_err(|e| OfferError::Template(format!("Logo lässt sich nicht lesen: {e}")))?;
        let fitted = img.resize(width, height, image::imageops::FilterType::Lanczos3);
        let (w, h) = fitted.dimensions();
        image::imageops::overlay(
            &mut canvas,
            &fitted.to_rgba8(),
            i64::from((width - w) / 2),
            i64::from((height - h) / 2),
        );
    }
    let mut out = Cursor::new(Vec::new());
    let image = DynamicImage::ImageRgba8(canvas);
    match format {
        ImageFormat::Jpeg => {
            // JPEG has no transparency: flatten onto white.
            let mut white = RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
            image::imageops::overlay(&mut white, &image.to_rgba8(), 0, 0);
            DynamicImage::ImageRgba8(white).to_rgb8().write_to(&mut out, ImageFormat::Jpeg)
        }
        _ => image.write_to(&mut out, ImageFormat::Png),
    }
    .map_err(|e| OfferError::Template(format!("Logo konnte nicht geschrieben werden: {e}")))?;
    Ok(out.into_inner())
}

/// Whether `bytes` is a logo the generator can place (PNG, JPEG or WebP).
pub fn check_logo(bytes: &[u8]) -> Result<(), String> {
    image::load_from_memory(bytes).map(|_| ()).map_err(|_| "kein lesbares PNG, JPEG oder WebP".to_string())
}

/// A copy of the `builtin` XLSX template with Aust's letterhead swapped for `lh`.
pub fn derive(builtin: &[u8], lh: &Letterhead) -> Result<Vec<u8>, OfferError> {
    let mut archive = ZipArchive::new(Cursor::new(builtin)).map_err(map_zip)?;
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let replacements = lh.replacements();
    let footer = lh.footer();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(map_zip)?;
        let name = entry.name().to_string();
        let options = SimpleFileOptions::default().compression_method(entry.compression());
        if entry.is_dir() {
            writer.add_directory(name, options).map_err(map_zip)?;
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(map_io)?;

        let data = if name.ends_with(".xml") || name.ends_with(".rels") {
            let mut text = String::from_utf8(data)
                .map_err(|e| OfferError::Template(format!("{name} is not UTF-8: {e}")))?;
            if name == "xl/styles.xml" {
                if let Some(argb) = accent_argb(&lh.accent) {
                    // Aust's orange (headers) and its lighter shade.
                    text = text.replace("FFFF6600", &argb).replace("FFFF9900", &argb);
                }
            }
            for (from, to) in &replacements {
                text = text.replace(from, to);
            }
            if text.contains("<oddFooter>") {
                text = replace_footers(&text, &footer);
            }
            text.into_bytes()
        } else if name.starts_with("xl/media/") {
            let format = ImageFormat::from_path(&name).unwrap_or(ImageFormat::Png);
            let (w, h) = image::load_from_memory(&data)
                .map(|img| img.dimensions())
                .map_err(|e| OfferError::Template(format!("{name}: {e}")))?;
            fit_logo(lh.logo.as_deref(), w, h, format)?
        } else {
            data
        };

        writer.start_file(name, options).map_err(map_zip)?;
        writer.write_all(&data).map_err(map_io)?;
    }
    Ok(writer.finish().map_err(map_zip)?.into_inner())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Everything that identifies Aust in its templates.
    pub(crate) const AUST_IDENTIFIERS: &[&str] = &[
        "aust-umzuege", "Aust Umz", "Ehrlicher", "Kaiserstr", "7558379", "DE67", "2595 0130",
        "259501300057453749", "NOLADE21HIK", "30/101/22146", "DE330779170", "Sparkasse",
        "Alex Aust", "bei Aust", "<t>Aust</t>", "Hildeshiem",
    ];

    pub(crate) fn zweite() -> Letterhead {
        let mut logo = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(40, 10, image::Rgba([0, 80, 200, 255])))
            .write_to(&mut logo, ImageFormat::Png)
            .unwrap();
        Letterhead {
            name: "Zweite Umzüge & Co. KG".into(),
            short_name: "Zweite".into(),
            owner_name: "Erika Zweite".into(),
            street: "Hauptstr. 7".into(),
            postal_code: "29221".into(),
            city: "Celle".into(),
            phone: "05141 123456".into(),
            email: "info@zweite.de".into(),
            website: "www.zweite.de".into(),
            agb_url: "www.zweite.de/agb".into(),
            bank_name: "Volksbank Celle".into(),
            iban: "DE02 1203 0000 0000 2020 51".into(),
            bic: "GENODEF1CEL".into(),
            tax_number: "17/200/12345".into(),
            vat_id: "DE123456789".into(),
            logo: Some(logo.into_inner()),
            accent: "#1f7a5a".into(),
        }
    }

    fn texts(xlsx: &[u8]) -> Vec<(String, String)> {
        let mut a = ZipArchive::new(Cursor::new(xlsx)).unwrap();
        (0..a.len())
            .filter_map(|i| {
                let mut e = a.by_index(i).unwrap();
                let n = e.name().to_string();
                if !(n.ends_with(".xml") || n.ends_with(".rels")) {
                    return None;
                }
                let mut s = String::new();
                e.read_to_string(&mut s).unwrap();
                Some((n, s))
            })
            .collect()
    }

    const TEMPLATES: [(&str, &[u8]); 3] = [
        ("offer", include_bytes!("../../../templates/offer_template.xlsx")),
        ("invoice", include_bytes!("../../../templates/Rechnung_Vorlage_v4.xlsx")),
        ("travel", include_bytes!("../../../templates/reisekosten_template.xlsx")),
    ];

    /// Nothing that identifies Aust survives in another company's templates, and
    /// that company's own details are in.
    #[test]
    fn derived_templates_carry_no_trace_of_aust() {
        let lh = zweite();
        for (label, builtin) in TEMPLATES {
            let derived = derive(builtin, &lh).unwrap();
            let all = texts(&derived);
            for (name, text) in &all {
                for id in AUST_IDENTIFIERS {
                    assert!(!text.contains(id), "{label} {name} still contains {id:?}");
                }
            }
            let joined: String = all.iter().map(|(_, t)| t.as_str()).collect();
            assert!(!joined.contains("FFFF6600"), "{label}: Aust's orange must be gone");
            assert!(joined.contains("Zweite Umzüge &amp; Co. KG"), "{label}: name missing");
            if label != "travel" {
                assert!(joined.contains("Volksbank Celle"), "{label}: bank missing");
                assert!(joined.contains("St.Nr.: 17/200/12345"), "{label}: tax number missing");
                assert!(joined.contains("&amp;&amp; Co. KG"), "{label}: & must be && in the footer");
            }
        }
    }

    /// The logo is replaced (same size as before), and without one the area is
    /// left empty instead of showing Aust's.
    #[test]
    fn logos_are_replaced_or_left_empty() {
        for (label, builtin) in TEMPLATES {
            let mut before = ZipArchive::new(Cursor::new(builtin)).unwrap();
            for with_logo in [true, false] {
                let lh = Letterhead { logo: if with_logo { zweite().logo } else { None }, ..zweite() };
                let derived = derive(builtin, &lh).unwrap();
                let mut after = ZipArchive::new(Cursor::new(derived.as_slice())).unwrap();
                for i in 0..before.len() {
                    let name = before.by_index(i).unwrap().name().to_string();
                    if !name.starts_with("xl/media/") || name.ends_with('/') {
                        continue;
                    }
                    let mut old = Vec::new();
                    before.by_name(&name).unwrap().read_to_end(&mut old).unwrap();
                    let mut new = Vec::new();
                    after.by_name(&name).unwrap().read_to_end(&mut new).unwrap();
                    assert_ne!(old, new, "{label} {name}: Aust's image must be gone");
                    let (o, n) = (image::load_from_memory(&old).unwrap(), image::load_from_memory(&new).unwrap());
                    assert_eq!(o.dimensions(), n.dimensions(), "{label} {name}: size kept");
                }
            }
        }
    }
}
