//! Slicer persistence: reading and writing slicerCache and slicer parts.
//! Timeline persistence: reading and writing timelineCache and timeline parts.
//!
//! A slicer in Excel consists of two parts:
//! - slicerCache (xl/slicerCaches/slicerCache{N}.xml): the filter definition with all items
//! - slicer (xl/slicers/slicer{N}.xml): the on-sheet view (anchor, size, visible columns)
//!
//! A timeline in Excel consists of two parts:
//! - timelineCache (xl/timelineCaches/timelineCache{N}.xml): the filter definition with all members
//! - timeline (xl/timelines/timeline{N}.xml): the on-sheet view (anchor, size, visible columns)
//!
//! Multiple slicers can share one cache if they filter the same table column.
//! Multiple timelines can share one cache if they filter the same time field.

use crate::{
    package::Rel,
    xml::{self, El},
};
use gridcraft_model::{Anchor, AnchorMode, Slicer, Timeline, Workbook};
use std::collections::HashMap;

/// Content type for slicer cache
pub const CT_SLICER_CACHE: &str = "application/vnd.ms-excel.slicerCache+xml";
/// Content type for slicer
pub const CT_SLICER: &str = "application/vnd.ms-excel.slicer+xml";
/// Content type for timeline cache
pub const CT_TIMELINE_CACHE: &str = "application/vnd.ms-excel.timelineCache+xml";
/// Content type for timeline
pub const CT_TIMELINE: &str = "application/vnd.ms-excel.timeline+xml";

/// State shared while reading slicer caches
#[expect(dead_code)]
pub struct SlicerCacheIn {
    pub name: String,
    pub source_name: String,   // workbook-scoped name of the table/range
    pub column: String,        // column name in the source
    pub items: Vec<String>,    // all distinct values
    pub selected: Vec<String>, // currently selected values (empty = all selected)
    pub cross_filter: bool,    // whether other slicers on same cache filter together
}

/// Read all slicer caches from the workbook relationships
pub fn read_slicer_caches(cx: &mut crate::read::Ctx<'_>, wb_rels: &[Rel]) -> HashMap<u32, SlicerCacheIn> {
    let mut caches = HashMap::new();
    for rel in wb_rels.iter().filter(|r| r.kind == "slicerCache" && !r.external) {
        if let Some(x) = cx.optional_xml(&rel.target).ok().flatten()
            && x.name == "slicerCacheDefinition"
            && let Some(cache) = parse_slicer_cache(cx, &x)
        {
            // cacheId is an attribute on the root
            if let Some(id) = x.attr_u32("cacheId") {
                caches.insert(id, cache);
            }
        }
    }
    caches
}

fn parse_slicer_cache(_cx: &mut crate::read::Ctx<'_>, x: &El) -> Option<SlicerCacheIn> {
    let name = x.attr("name").unwrap_or("").to_string();
    let source_name = x.child("slicerCacheData").and_then(|d| d.attr("name")).unwrap_or("").to_string();

    let mut column = String::new();
    let mut items = Vec::new();
    let mut selected = Vec::new();

    // Read column from <columns> / <column name="...">
    if let Some(cols) = x.child("columns")
        && let Some(col) = cols.child("column")
    {
        column = col.attr("name").unwrap_or("").to_string();
    }

    // Read items from <slicerCacheLevels> / <slicerCacheLevel> / <si v="...">
    if let Some(levels) = x.child("slicerCacheLevels")
        && let Some(level) = levels.child("slicerCacheLevel")
    {
        for si in level.kids("si") {
            if let Some(v) = si.attr("v") {
                items.push(v.to_string());
            }
        }
    }

    // Read selected items from <slicerCacheLevels> / <slicerCacheLevel> / <si x="1"> (selected)
    if let Some(levels) = x.child("slicerCacheLevels")
        && let Some(level) = levels.child("slicerCacheLevel")
    {
        for si in level.kids("si") {
            if si.flag("x", false)
                && let Some(v) = si.attr("v")
            {
                selected.push(v.to_string());
            }
        }
    }

    Some(SlicerCacheIn { name, source_name, column, items, selected, cross_filter: x.flag("crossFilter", false) })
}

/// State shared while reading timeline caches
#[expect(dead_code)]
pub struct TimelineCacheIn {
    pub name: String,
    pub source_name: String,   // workbook-scoped name of the timeline field
    pub data_source: String,   // cache source reference
    pub items: Vec<String>,    // all distinct values/members
    pub selected: Vec<String>, // currently selected values (empty = all selected)
    pub cross_filter: bool,    // whether other timelines on same cache filter together
}

/// Read all timeline caches from the workbook relationships
pub fn read_timeline_caches(cx: &mut crate::read::Ctx<'_>, wb_rels: &[Rel]) -> HashMap<u32, TimelineCacheIn> {
    let mut caches = HashMap::new();
    for rel in wb_rels.iter().filter(|r| r.kind == "timelineCache" && !r.external) {
        if let Some(x) = cx.optional_xml(&rel.target).ok().flatten()
            && x.name == "timelineCacheDefinition"
            && let Some(cache) = parse_timeline_cache(cx, &x)
        {
            // cacheId is an attribute on the root
            if let Some(id) = x.attr_u32("cacheId") {
                caches.insert(id, cache);
            }
        }
    }
    caches
}

fn parse_timeline_cache(_cx: &mut crate::read::Ctx<'_>, x: &El) -> Option<TimelineCacheIn> {
    let name = x.attr("name").unwrap_or("").to_string();
    let source_name = x.child("timelineCacheData").and_then(|d| d.attr("name")).unwrap_or("").to_string();

    let mut data_source = String::new();
    let mut items = Vec::new();
    let mut selected = Vec::new();

    // Read data source from <timelineCacheData>
    if let Some(cd) = x.child("timelineCacheData") {
        data_source = cd.attr("source").unwrap_or("").to_string();
        // Read items from <timelineCacheLevels> / <timelineCacheLevel> / <draft> / <member>
        if let Some(levels) = cd.child("timelineCacheLevels") {
            if let Some(level) = levels.child("timelineCacheLevel") {
                for mi in level.kids("member") {
                    if let Some(v) = mi.attr("uid") {
                        items.push(v.to_string());
                    }
                }
                // Read selected members from <selectedTimelineCacheLevel> / <item>
                if let Some(sel) = cd.child("selectedTimelineCacheLevel") {
                    for item in sel.kids("item") {
                        if item.flag("selected", false)
                            && let Some(v) = item.attr("uid")
                        {
                            selected.push(v.to_string());
                        }
                    }
                }
            }
        }
    }

    Some(TimelineCacheIn { name, source_name, data_source, items, selected, cross_filter: x.flag("crossFilter", false) })
}

/// Read all timelines for a specific sheet
pub fn read_timelines(cx: &mut crate::read::Ctx<'_>, sheet_part: &str, caches: &HashMap<u32, TimelineCacheIn>) -> Vec<Timeline> {
    let rels = match cx.pkg.rels(sheet_part) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let mut timelines = Vec::new();
    for rel in rels.iter().filter(|r| r.kind == "timeline" && !r.external) {
        if let Some(x) = cx.optional_xml(&rel.target).ok().flatten()
            && x.name == "timeline"
            && let Some(timeline) = parse_timeline(cx, &x, caches)
        {
            timelines.push(timeline);
        }
    }
    timelines
}

fn parse_timeline(_cx: &mut crate::read::Ctx<'_>, x: &El, caches: &HashMap<u32, TimelineCacheIn>) -> Option<Timeline> {
    let cache_id = x.attr_u32("cacheId")?;
    let cache = caches.get(&cache_id)?;

    let id = x.attr_u32("id").unwrap_or(0);
    let name = x.attr("name").unwrap_or("").to_string();

    // Parse anchor from <position> or <x:shape>
    let anchor = parse_timeline_anchor(x)?;

    // Read selected members from <state> or from the cache
    let selected = if let Some(state) = x.child("state") {
        let mut sel = Vec::new();
        for item in state.kids("item") {
            if item.flag("selected", false)
                && let Some(v) = item.attr("uid")
            {
                sel.push(v.to_string());
            }
        }
        if sel.is_empty() { None } else { Some(sel) }
    } else if cache.selected.is_empty() {
        None
    } else {
        Some(cache.selected.clone())
    };

    let columns = x.attr_u32("columns").unwrap_or(1);
    let rows = x.attr_u32("rows").unwrap_or(1);

    Some(Timeline { id, table: cache.source_name.clone(), column: cache.data_source.clone(), caption: name, data_source: cache.data_source.clone(), anchor, selected, columns, rows })
}

fn parse_timeline_anchor(x: &El) -> Option<Anchor> {
    // Try <position> first (simpler format)
    if let Some(pos) = x.child("position") {
        let r = pos.attr_u32("r").unwrap_or(0);
        let c = pos.attr_u32("c").unwrap_or(0);
        let width = pos.attr_f64("width").unwrap_or(168.0) as f32;
        let height = pos.attr_f64("height").unwrap_or(200.0) as f32;
        return Some(Anchor { cell: gridcraft_core::CellRef::new(r, c), dx: 4.0, dy: 4.0, width, height, mode: AnchorMode::MoveOnly });
    }

    // Try <x:shape> (VML-based anchor, more complex)
    if let Some(shape) = x.child("shape") {
        let style = shape.attr("style").unwrap_or("");
        let client_data = shape.child("ClientData");
        let row = client_data.and_then(|cd| cd.child("Row")).and_then(|r| r.text.trim().parse::<u32>().ok()).unwrap_or(0);
        let col = client_data.and_then(|cd| cd.child("Column")).and_then(|c| c.text.trim().parse::<u32>().ok()).unwrap_or(0);

        // Parse width/height from style
        let width = parse_style_dimension(style, "width").unwrap_or(168.0) as f32;
        let height = parse_style_dimension(style, "height").unwrap_or(200.0) as f32;

        return Some(Anchor { cell: gridcraft_core::CellRef::new(row, col), dx: 4.0, dy: 4.0, width, height, mode: AnchorMode::MoveOnly });
    }

    // Default
    Some(Anchor { cell: gridcraft_core::CellRef::new(0, 0), dx: 4.0, dy: 4.0, width: 168.0, height: 200.0, mode: AnchorMode::MoveOnly })
}

/// Read all slicers for a specific sheet
pub fn read_slicers(cx: &mut crate::read::Ctx<'_>, sheet_part: &str, caches: &HashMap<u32, SlicerCacheIn>) -> Vec<Slicer> {
    let rels = match cx.pkg.rels(sheet_part) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let mut slicers = Vec::new();
    for rel in rels.iter().filter(|r| r.kind == "slicer" && !r.external) {
        if let Some(x) = cx.optional_xml(&rel.target).ok().flatten()
            && x.name == "slicer"
            && let Some(slicer) = parse_slicer(cx, &x, caches)
        {
            slicers.push(slicer);
        }
    }
    slicers
}

fn parse_slicer(_cx: &mut crate::read::Ctx<'_>, x: &El, caches: &HashMap<u32, SlicerCacheIn>) -> Option<Slicer> {
    let cache_id = x.attr_u32("cacheId")?;
    let cache = caches.get(&cache_id)?;

    let id = x.attr_u32("id").unwrap_or(0);
    let name = x.attr("name").unwrap_or("").to_string();

    // Parse anchor from <x:shape> or <position>
    let anchor = parse_slicer_anchor(x)?;

    // Read selected items from <state> or from the cache
    let selected = if let Some(state) = x.child("state") {
        let mut sel = Vec::new();
        for item in state.kids("item") {
            if item.flag("selected", false)
                && let Some(v) = item.attr("v")
            {
                sel.push(v.to_string());
            }
        }
        if sel.is_empty() { None } else { Some(sel) }
    } else if cache.selected.is_empty() {
        None
    } else {
        Some(cache.selected.clone())
    };

    let columns = x.attr_u32("columns").unwrap_or(1);

    Some(Slicer { id, table: cache.source_name.clone(), column: cache.column.clone(), caption: name, anchor, selected, columns })
}

fn parse_slicer_anchor(x: &El) -> Option<Anchor> {
    // Try <position> first (simpler format)
    if let Some(pos) = x.child("position") {
        let r = pos.attr_u32("r").unwrap_or(0);
        let c = pos.attr_u32("c").unwrap_or(0);
        let width = pos.attr_f64("width").unwrap_or(168.0) as f32;
        let height = pos.attr_f64("height").unwrap_or(200.0) as f32;
        return Some(Anchor { cell: gridcraft_core::CellRef::new(r, c), dx: 4.0, dy: 4.0, width, height, mode: AnchorMode::MoveOnly });
    }

    // Try <x:shape> (VML-based anchor, more complex)
    if let Some(shape) = x.child("shape") {
        let style = shape.attr("style").unwrap_or("");
        let client_data = shape.child("ClientData");
        let row = client_data.and_then(|cd| cd.child("Row")).and_then(|r| r.text.trim().parse::<u32>().ok()).unwrap_or(0);
        let col = client_data.and_then(|cd| cd.child("Column")).and_then(|c| c.text.trim().parse::<u32>().ok()).unwrap_or(0);

        // Parse width/height from style
        let width = parse_style_dimension(style, "width").unwrap_or(168.0) as f32;
        let height = parse_style_dimension(style, "height").unwrap_or(200.0) as f32;

        return Some(Anchor { cell: gridcraft_core::CellRef::new(row, col), dx: 4.0, dy: 4.0, width, height, mode: AnchorMode::MoveOnly });
    }

    // Default
    Some(Anchor { cell: gridcraft_core::CellRef::new(0, 0), dx: 4.0, dy: 4.0, width: 168.0, height: 200.0, mode: AnchorMode::MoveOnly })
}

fn parse_style_dimension(style: &str, key: &str) -> Option<f64> {
    for part in style.split(';') {
        if let Some((k, v)) = part.split_once(':')
            && k.trim().eq_ignore_ascii_case(key)
        {
            return v.trim().strip_suffix("pt")?.trim().parse::<f64>().ok();
        }
    }
    None
}

/// Get all distinct values from a table column for the slicer cache
fn get_slicer_items(wb: &Workbook, table_name: &str, column_name: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();

    for sheet in &wb.sheets {
        if let Some(table) = sheet.tables.iter().find(|t| t.name.eq_ignore_ascii_case(table_name))
            && let Some(col_idx) = table.column_index(column_name)
            && let Some(data_range) = table.data_range()
        {
            let col = data_range.start.col + col_idx as u32;
            for row in data_range.start.row..=data_range.end.row {
                let cell_ref = gridcraft_core::CellRef::new(row, col);
                if let Some(cell) = sheet.cell(cell_ref) {
                    let text = match &cell.value {
                        gridcraft_core::Value::Empty => String::new(),
                        gridcraft_core::Value::Number(n) => gridcraft_core::number_to_text(*n),
                        gridcraft_core::Value::Text(t) => t.as_ref().to_string(),
                        gridcraft_core::Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
                        gridcraft_core::Value::Error(e) => (*e).as_str().to_string(),
                        gridcraft_core::Value::Array(_) => String::new(),
                    };
                    if !text.is_empty() && seen.insert(text.to_lowercase()) {
                        out.push(text);
                    }
                }
            }
        }
    }
    out
}
/// Write all slicers from the workbook
pub fn write_slicers(wb: &Workbook, out: &mut crate::write::Out, wb_rels: &mut crate::write::Rels) -> HashMap<(usize, usize), u32> {
    // (sheet_idx, slicer_idx) -> slicer_id
    let mut cache_id = 1;
    let mut slicer_id = 1;
    let mut cache_map: HashMap<(String, String), u32> = HashMap::new(); // (table, column) -> cache_id
    let mut slicer_id_map: HashMap<(usize, usize), u32> = HashMap::new();

    // First pass: collect all unique (table, column) pairs and write caches
    for sheet in &wb.sheets {
        for slicer in &sheet.slicers {
            let key = (slicer.table.clone(), slicer.column.clone());
            if !cache_map.contains_key(&key) {
                cache_map.insert(key.clone(), cache_id);

                // Get items from the table column
                let items = get_slicer_items(wb, &slicer.table, &slicer.column);

                let cache_xml = build_slicer_cache_xml(slicer, cache_id, &items);
                let cache_part = format!("xl/slicerCaches/slicerCache{cache_id}.xml");
                out.part(&cache_part, Some(CT_SLICER_CACHE), cache_xml.into_bytes());
                wb_rels.add("slicerCache", &format!("slicerCaches/slicerCache{cache_id}.xml"));
                cache_id += 1;
            }
        }
    }

    // Second pass: write slicer parts
    for (si, sheet) in wb.sheets.iter().enumerate() {
        for (slicer_idx, slicer) in sheet.slicers.iter().enumerate() {
            let key = (slicer.table.clone(), slicer.column.clone());
            let cid = cache_map[&key];

            let slicer_xml = build_slicer_xml(slicer, slicer_id, cid);
            let slicer_part = format!("xl/slicers/slicer{slicer_id}.xml");
            out.part(&slicer_part, Some(CT_SLICER), slicer_xml.into_bytes());

            slicer_id_map.insert((si, slicer_idx), slicer_id);
            slicer_id += 1;
        }
    }

    slicer_id_map
}

/// Write all timelines from the workbook
pub fn write_timelines(wb: &Workbook, out: &mut crate::write::Out, wb_rels: &mut crate::write::Rels) -> HashMap<(usize, usize), u32> {
    // (sheet_idx, timeline_idx) -> timeline_id
    let mut cache_id = 1;
    let mut timeline_id = 1;
    let mut cache_map: HashMap<(String, String), u32> = HashMap::new(); // (table, data_source) -> cache_id
    let mut timeline_id_map: HashMap<(usize, usize), u32> = HashMap::new();

    // First pass: collect all unique (table, data_source) pairs and write caches
    for sheet in &wb.sheets {
        for timeline in &sheet.timelines {
            let key = (timeline.table.clone(), timeline.data_source.clone());
            if !cache_map.contains_key(&key) {
                cache_map.insert(key.clone(), cache_id);

                // Get items from the timeline data source
                let items = get_timeline_items(wb, &timeline.table, &timeline.data_source);

                let cache_xml = build_timeline_cache_xml(timeline, cache_id, &items);
                let cache_part = format!("xl/timelineCaches/timelineCache{cache_id}.xml");
                out.part(&cache_part, Some(CT_TIMELINE_CACHE), cache_xml.into_bytes());
                wb_rels.add("timelineCache", &format!("timelineCaches/timelineCache{cache_id}.xml"));
                cache_id += 1;
            }
        }
    }

    // Second pass: write timeline parts
    for (ti, sheet) in wb.sheets.iter().enumerate() {
        for (timeline_idx, timeline) in sheet.timelines.iter().enumerate() {
            let key = (timeline.table.clone(), timeline.data_source.clone());
            let cid = cache_map[&key];

            let timeline_xml = build_timeline_xml(timeline, timeline_id, cid);
            let timeline_part = format!("xl/timelines/timeline{timeline_id}.xml");
            out.part(&timeline_part, Some(CT_TIMELINE), timeline_xml.into_bytes());

            timeline_id_map.insert((ti, timeline_idx), timeline_id);
            timeline_id += 1;
        }
    }

    timeline_id_map
}

/// Get all distinct values from a table column for the timeline cache
fn get_timeline_items(wb: &Workbook, table_name: &str, data_source: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();

    for sheet in &wb.sheets {
        if let Some(table) = sheet.tables.iter().find(|t| t.name.eq_ignore_ascii_case(table_name))
            && let Some(col_idx) = table.column_index(data_source)
        {
            // For timeline, data_source is typically a time field name
            // Read unique values from the column by iterating data range rows
            if let Some(data_range) = table.data_range() {
                let col = data_range.start.col + col_idx as u32;
                for row in data_range.start.row..=data_range.end.row {
                    let cell_ref = gridcraft_core::CellRef::new(row, col);
                    if let Some(cell) = sheet.cell(cell_ref) {
                        let text = match &cell.value {
                            gridcraft_core::Value::Empty => String::new(),
                            gridcraft_core::Value::Number(n) => gridcraft_core::number_to_text(*n),
                            gridcraft_core::Value::Text(t) => t.as_ref().to_string(),
                            gridcraft_core::Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
                            gridcraft_core::Value::Error(e) => (*e).as_str().to_string(),
                            gridcraft_core::Value::Array(_) => String::new(),
                        };
                        if !text.is_empty() && seen.insert(text.to_lowercase()) {
                            out.push(text);
                        }
                    }
                }
            }
        }
    }
    out
}

fn build_timeline_cache_xml(timeline: &Timeline, cache_id: u32, items: &[String]) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>");
    xml.push_str(&format!("<timelineCacheDefinition xmlns=\"http://schemas.microsoft.com/office/2007/excel\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" name=\"{}\" cacheId=\"{}\" crossFilter=\"0\" showHeader=\"1\">",
        xml::esc_attr(&timeline.caption), cache_id));

    // timelineCacheData - source reference
    xml.push_str(&format!("<timelineCacheData source=\"{}\" olap=\"0\"/>", xml::esc_attr(&timeline.data_source)));

    // members - which field is filtered
    xml.push_str("<members count=\"1\">");
    xml.push_str(&format!("<member uniqueName=\"{}\"/>", xml::esc_attr(&timeline.data_source)));
    xml.push_str("</members>");

    // timelineCacheLevels - all distinct members
    xml.push_str("<timelineCacheLevels count=\"1\">");
    xml.push_str(&format!("<timelineCacheLevel uniqueCount=\"{}\" minValue=\"\" maxValue=\"\">", items.len()));
    for item in items {
        xml.push_str(&format!("<draft v=\"{}\"/>", xml::esc_attr(item)));
    }
    xml.push_str("</timelineCacheLevel>");
    xml.push_str("</timelineCacheLevels>");

    // style
    xml.push_str("<style name=\"TimelineStyle1\"/>");

    xml.push_str("</timelineCacheDefinition>");
    xml
}

pub fn build_timeline_xml(timeline: &Timeline, timeline_id: u32, cache_id: u32) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>");
    xml.push_str(&format!("<timeline xmlns=\"http://schemas.microsoft.com/office/2007/excel\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" name=\"{}\" id=\"{}\" cacheId=\"{}\" rows=\"{}\">",
        xml::esc_attr(&timeline.caption), timeline_id, cache_id, timeline.rows));

    // position
    let cell = &timeline.anchor.cell;
    xml.push_str(&format!("<position r=\"{}\" c=\"{}\" width=\"{}\" height=\"{}\"/>", cell.row, cell.col, timeline.anchor.width, timeline.anchor.height));

    // state - selected members
    if let Some(sel) = &timeline.selected {
        xml.push_str("<state>");
        for item in sel {
            xml.push_str(&format!("<member uid=\"{}\" selected=\"1\"/>", xml::esc_attr(item)));
        }
        xml.push_str("</state>");
    }

    xml.push_str("</timeline>");
    xml
}

fn build_slicer_cache_xml(slicer: &Slicer, cache_id: u32, items: &[String]) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>");
    xml.push_str(&format!("<slicerCacheDefinition xmlns=\"http://schemas.microsoft.com/office/2007/excel\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" name=\"{}\" cacheId=\"{}\" crossFilter=\"0\" showHeader=\"1\">",
        xml::esc_attr(&slicer.caption), cache_id));

    // slicerCacheData - source reference
    xml.push_str(&format!("<slicerCacheData name=\"{}\" olap=\"0\"/>", xml::esc_attr(&slicer.table)));

    // columns - which column is filtered
    xml.push_str("<columns count=\"1\">");
    xml.push_str(&format!("<column name=\"{}\"/>", xml::esc_attr(&slicer.column)));
    xml.push_str("</columns>");

    // slicerCacheLevels - all distinct values
    xml.push_str("<slicerCacheLevels count=\"1\">");
    xml.push_str(&format!("<slicerCacheLevel uniqueCount=\"{}\" minValue=\"\" maxValue=\"\">", items.len()));
    for item in items {
        xml.push_str(&format!("<si v=\"{}\"/>", xml::esc_attr(item)));
    }
    xml.push_str("</slicerCacheLevel>");
    xml.push_str("</slicerCacheLevels>");

    // style
    xml.push_str("<style name=\"SlicerStyleLight1\"/>");

    xml.push_str("</slicerCacheDefinition>");
    xml
}

pub fn build_slicer_xml(slicer: &Slicer, slicer_id: u32, cache_id: u32) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>");
    xml.push_str(&format!("<slicer xmlns=\"http://schemas.microsoft.com/office/2007/excel\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" name=\"{}\" id=\"{}\" cacheId=\"{}\" columns=\"{}\">",
        xml::esc_attr(&slicer.caption), slicer_id, cache_id, slicer.columns));

    // position
    let cell = &slicer.anchor.cell;
    xml.push_str(&format!("<position r=\"{}\" c=\"{}\" width=\"{}\" height=\"{}\"/>", cell.row, cell.col, slicer.anchor.width, slicer.anchor.height));

    // state - selected items
    if let Some(sel) = &slicer.selected {
        xml.push_str("<state>");
        for item in sel {
            xml.push_str(&format!("<item v=\"{}\" selected=\"1\"/>", xml::esc_attr(item)));
        }
        xml.push_str("</state>");
    }

    xml.push_str("</slicer>");
    xml
}
