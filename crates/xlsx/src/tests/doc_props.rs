//! Original, code-built metadata packages; no external workbook bytes are embedded.

use gridcraft_model::DocProps;

use super::{CT, make_zip};
use crate::{read_xlsx, write_xlsx};

fn metadata_book(strict: bool, core: &str, app: &str) -> Vec<u8> {
    let office = if strict {
        "http://purl.oclc.org/ooxml/officeDocument/relationships"
    } else {
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
    };
    let app_kind = if strict { "extendedProperties" } else { "extended-properties" };
    let root = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{office}/officeDocument" Target="xl/workbook.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="{office}/{app_kind}" Target="docProps/app.xml"/></Relationships>"#
    );
    let rels = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{office}/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#
    );
    let main = if strict { "http://purl.oclc.org/ooxml/spreadsheetml/main" } else { "http://schemas.openxmlformats.org/spreadsheetml/2006/main" };
    let wb = format!(r#"<workbook xmlns="{main}" xmlns:r="{office}"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#);
    let sheet = format!(r#"<worksheet xmlns="{main}"><sheetData/></worksheet>"#);
    make_zip(&[
        ("[Content_Types].xml", CT.as_bytes()),
        ("_rels/.rels", root.as_bytes()),
        ("xl/workbook.xml", wb.as_bytes()),
        ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
        ("docProps/core.xml", core.as_bytes()),
        ("docProps/app.xml", app.as_bytes()),
    ])
}

#[test]
fn source_core_metadata_survives_transitional_and_strict_round_trip() {
    let core = r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>  Title &amp; details  </dc:title><dc:subject>Subject</dc:subject><dc:creator>Test author</dc:creator><cp:keywords>a, b</cp:keywords><dc:description>Description</dc:description><cp:lastModifiedBy>Test editor</cp:lastModifiedBy><dcterms:created xsi:type="dcterms:W3CDTF">2025-01-02T03:04:05Z</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">2025-02-03T04:05:06+03:00</dcterms:modified><cp:lastPrinted>2025-02-02T00:00:00Z</cp:lastPrinted><cp:revision>42</cp:revision><dc:language>ru-RU</dc:language><dc:identifier>book-123</dc:identifier><cp:category>Financial</cp:category><cp:contentStatus>Draft</cp:contentStatus><cp:version>1.2</cp:version></cp:coreProperties>"#;
    for strict in [false, true] {
        let namespace = if strict {
            "http://purl.oclc.org/ooxml/officeDocument/extendedProperties"
        } else {
            "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"
        };
        let app = format!(r#"<Properties xmlns="{namespace}"><Company>Test &amp; Co</Company></Properties>"#);
        let (wb, _) = read_xlsx(&metadata_book(strict, core, &app)).unwrap();
        let expected = DocProps {
            title: "  Title & details  ".into(),
            subject: "Subject".into(),
            author: "Test author".into(),
            company: "Test & Co".into(),
            keywords: "a, b".into(),
            description: "Description".into(),
            last_modified_by: "Test editor".into(),
            created: "2025-01-02T03:04:05Z".into(),
            modified: "2025-02-03T04:05:06+03:00".into(),
            last_printed: "2025-02-02T00:00:00Z".into(),
            revision: "42".into(),
            language: "ru-RU".into(),
            identifier: "book-123".into(),
            category: "Financial".into(),
            content_status: "Draft".into(),
            version: "1.2".into(),
        };
        assert_eq!(wb.props, expected, "strict={strict}");
        let bytes = write_xlsx(&wb).unwrap();
        let (back, _) = read_xlsx(&bytes).unwrap();
        assert_eq!(back.props, expected, "strict={strict}");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let mut written = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("docProps/core.xml").unwrap(), &mut written).unwrap();
        assert!(written.contains(r#"<dcterms:created xsi:type="dcterms:W3CDTF">2025-01-02T03:04:05Z</dcterms:created>"#));
        assert!(written.contains(r#"<dcterms:modified xsi:type="dcterms:W3CDTF">2025-02-03T04:05:06+03:00</dcterms:modified>"#));
    }
}
