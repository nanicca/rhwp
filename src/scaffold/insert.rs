//! 열린 문서에 [`Block`] 시퀀스를 끼워 넣는다 — `scaffold` 의 **무(無)에서 생성** 축을
//! **기존 문서 편집** 축으로 확장한다.
//!
//! `build_scaffold` 는 자기가 만든 `doc_info`(글자·문단 모양 ID 0~8, 테두리 1~2)를 전제로
//! 문단을 조립한다. 파싱해 들어온 문서는 그 전제가 성립하지 않으므로, 여기서는 삽입
//! 지점 **앞 문단의 서식을 상속**하고(한글에서 Enter 를 친 것과 같은 결과) 개요 제목·
//! 표 테두리처럼 문서에 없을 수 있는 모양만 `doc_info` 에 **덧붙여** 만든다. 기존 항목은
//! 하나도 고치지 않는다 — 원본의 다른 문단이 참조하는 모양이 바뀌면 편집하지 않은 쪽의
//! 조판까지 흔들린다.
//!
//! 삽입 뒤 파생 상태는 [`DocumentCore::rebuild_derived_state`] 에 통째로 맡긴다. 여러
//! 문단을 한 번에 넣으므로 문단 단위 증분 재조판을 반복하는 것보다 싸고, 새 `doc_info`
//! 항목까지 스타일 해소에 반영되는 유일한 경로다.

use std::collections::HashMap;

use crate::document_core::DocumentCore;
use crate::error::HwpError;
use crate::model::paragraph::Paragraph;
use crate::model::style::{BorderFill, BorderLine, BorderLineType, HeadType};
use crate::scaffold::builder::{build_table_paragraph_with, content_width_of, make_text_para};
use crate::scaffold::markdown::parse_markdown_blocks;
use crate::scaffold::schema::Block;

/// [`DocumentCore::insert_blocks_native`] 의 결과 — 봉투는 호출자가 만든다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsertBlocksReport {
    /// 삽입한 구역.
    pub section: usize,
    /// 새 문단이 시작하는 인덱스 (삽입 전 `at`).
    pub first_paragraph: usize,
    /// 실제로 늘어난 문단 수 — 표 블록은 표 문단 + 뒤따르는 빈 문단으로 2 다.
    pub inserted_paragraphs: usize,
    pub heading_count: usize,
    pub paragraph_count: usize,
    pub table_count: usize,
    /// 삽입 후 구역의 문단 수.
    pub paragraph_count_after: usize,
}

impl DocumentCore {
    /// Markdown 텍스트를 블록으로 풀어 [`Self::insert_blocks_native`] 로 넘긴다.
    pub fn insert_markdown_native(
        &mut self,
        section_idx: usize,
        at: Option<usize>,
        markdown: &str,
    ) -> Result<InsertBlocksReport, HwpError> {
        let blocks = parse_markdown_blocks(markdown);
        self.insert_blocks_native(section_idx, at, &blocks)
    }

    /// `section_idx` 구역의 `at` 위치(생략 시 구역 끝)에 블록들을 문단으로 끼워 넣는다.
    ///
    /// `at == paragraphs.len()` 은 append 다. 그 이상은 범위 오류. 빈 블록 목록은 문서를
    /// 건드리지 않고 0 건 보고로 돌아간다.
    pub fn insert_blocks_native(
        &mut self,
        section_idx: usize,
        at: Option<usize>,
        blocks: &[Block],
    ) -> Result<InsertBlocksReport, HwpError> {
        let section_count = self.document.sections.len();
        if section_idx >= section_count {
            return Err(HwpError::RenderError(format!(
                "구역 인덱스 {section_idx} 범위 초과 (총 {section_count}개)"
            )));
        }
        let para_count = self.document.sections[section_idx].paragraphs.len();
        let at = at.unwrap_or(para_count);
        if at > para_count {
            return Err(HwpError::RenderError(format!(
                "문단 인덱스 {at} 범위 초과 (총 {para_count}개, 최대 {para_count})"
            )));
        }
        if blocks.is_empty() {
            return Ok(InsertBlocksReport {
                section: section_idx,
                first_paragraph: at,
                inserted_paragraphs: 0,
                heading_count: 0,
                paragraph_count: 0,
                table_count: 0,
                paragraph_count_after: para_count,
            });
        }

        // ── 서식 상속원 — 삽입 지점 앞 문단(없으면 밀려날 현재 문단, 그것도 없으면 기본). ──
        let template: Option<Paragraph> = {
            let paragraphs = &self.document.sections[section_idx].paragraphs;
            paragraphs
                .get(at.saturating_sub(1))
                .or_else(|| paragraphs.first())
                .cloned()
        };
        let mut styles = StyleResolver::new(self, template.as_ref());

        let content_width =
            content_width_of(&self.document.sections[section_idx].section_def.page_def);

        let mut new_paras: Vec<Paragraph> = Vec::new();
        let (mut headings, mut paragraphs, mut tables) = (0usize, 0usize, 0usize);
        for block in blocks {
            match block {
                Block::Heading { level, text } => {
                    let level = (*level).clamp(1, 7);
                    let ps = styles.heading_para_shape(&mut self.document, level);
                    let cs = styles.heading_char_shape(&mut self.document);
                    let mut para = make_text_para(text, ps, cs);
                    para.style_id = styles.style_id;
                    new_paras.push(para);
                    headings += 1;
                }
                Block::Paragraph { text } => {
                    let mut para = make_text_para(text, styles.normal_ps, styles.normal_cs);
                    para.style_id = styles.style_id;
                    new_paras.push(para);
                    paragraphs += 1;
                }
                Block::Table { rows } => {
                    let bf = styles.solid_border_fill(&mut self.document);
                    if let Some(mut table_para) =
                        build_table_paragraph_with(rows, content_width, bf, styles.normal_cs)
                    {
                        table_para.para_shape_id = styles.normal_ps;
                        table_para.style_id = styles.style_id;
                        new_paras.push(table_para);
                        // 표 문단 뒤에는 평문 문단이 온다(한컴 표준 구조 + 다음 표와의 경계).
                        let mut trailer = make_text_para("", styles.normal_ps, styles.normal_cs);
                        trailer.has_para_text = false;
                        trailer.style_id = styles.style_id;
                        new_paras.push(trailer);
                        tables += 1;
                    }
                }
            }
        }
        let inserted = new_paras.len();
        if inserted == 0 {
            return Ok(InsertBlocksReport {
                section: section_idx,
                first_paragraph: at,
                inserted_paragraphs: 0,
                heading_count: 0,
                paragraph_count: 0,
                table_count: 0,
                paragraph_count_after: para_count,
            });
        }

        let section = &mut self.document.sections[section_idx];
        // 원본 스트림 지름길은 더는 유효하지 않다 — 직렬화기가 IR 을 다시 쓰게 한다.
        section.raw_stream = None;
        // 구역의 첫 문단은 "여기서 구역이 시작한다"는 표식을 지닌다(자리에 딸린 속성).
        // 앞에 끼우면 새 첫 문단으로 옮긴다 — `insert_paragraph_native` 와 같은 규칙.
        if at == 0 {
            if let Some(displaced) = section.paragraphs.first_mut() {
                let column_type = std::mem::take(&mut displaced.column_type);
                let raw_break_type = std::mem::take(&mut displaced.raw_break_type);
                if let Some(new_first) = new_paras.first_mut() {
                    new_first.column_type = column_type;
                    new_first.raw_break_type = raw_break_type;
                }
            }
        }
        section.paragraphs.splice(at..at, new_paras);
        let paragraph_count_after = section.paragraphs.len();

        self.rebuild_derived_state();

        Ok(InsertBlocksReport {
            section: section_idx,
            first_paragraph: at,
            inserted_paragraphs: inserted,
            heading_count: headings,
            paragraph_count: paragraphs,
            table_count: tables,
            paragraph_count_after,
        })
    }
}

/// 삽입 문단이 참조할 `doc_info` ID 들 — 상속으로 정하고, 없는 모양만 덧붙인다.
struct StyleResolver {
    normal_ps: u16,
    normal_cs: u32,
    style_id: u8,
    heading_ps: HashMap<u8, u16>,
    heading_cs: Option<u32>,
    solid_bf: Option<u16>,
}

impl StyleResolver {
    fn new(core: &DocumentCore, template: Option<&Paragraph>) -> Self {
        let doc = &core.document;
        let style0 = doc.doc_info.styles.first();
        let mut normal_ps = template
            .map(|p| p.para_shape_id)
            .or_else(|| style0.map(|s| s.para_shape_id))
            .unwrap_or(0);
        let normal_cs = template
            .and_then(|p| p.char_shapes.last().map(|c| c.char_shape_id))
            .or_else(|| style0.map(|s| s.char_shape_id as u32))
            .unwrap_or(0);
        let style_id = template.map(|p| p.style_id).unwrap_or(0);
        // 상속원이 개요 제목이면 본문 문단까지 제목이 된다 — 바탕글 문단 모양으로 되돌린다.
        let inherited_is_outline = doc
            .doc_info
            .para_shapes
            .get(normal_ps as usize)
            .is_some_and(|ps| ps.head_type == HeadType::Outline);
        if inherited_is_outline {
            if let Some(s) = style0 {
                normal_ps = s.para_shape_id;
            }
        }
        StyleResolver {
            normal_ps,
            normal_cs,
            style_id,
            heading_ps: HashMap::new(),
            heading_cs: None,
            solid_bf: None,
        }
    }

    /// 개요 수준 `level`(1~7) 의 문단 모양 — 문서에 이미 있으면 재사용, 없으면 바탕 문단
    /// 모양을 복제해 `head_type=Outline, para_level=level-1` 로 덧붙인다.
    fn heading_para_shape(&mut self, doc: &mut crate::model::document::Document, level: u8) -> u16 {
        if let Some(id) = self.heading_ps.get(&level) {
            return *id;
        }
        let existing = doc
            .doc_info
            .para_shapes
            .iter()
            .position(|ps| ps.head_type == HeadType::Outline && ps.para_level == level - 1);
        let id = match existing {
            Some(idx) => idx as u16,
            None => {
                let mut ps = doc
                    .doc_info
                    .para_shapes
                    .get(self.normal_ps as usize)
                    .cloned()
                    .unwrap_or_default();
                ps.raw_data = None;
                ps.head_type = HeadType::Outline;
                ps.para_level = level - 1;
                ps.numbering_id = 0;
                if ps.spacing_before == 0 {
                    ps.spacing_before = 200;
                }
                if ps.spacing_after == 0 {
                    ps.spacing_after = 100;
                }
                doc.doc_info.para_shapes.push(ps);
                (doc.doc_info.para_shapes.len() - 1) as u16
            }
        };
        self.heading_ps.insert(level, id);
        id
    }

    /// 제목 글자 모양 — 바탕 글자 모양을 복제해 굵게로 덧붙인다(한 번만).
    fn heading_char_shape(&mut self, doc: &mut crate::model::document::Document) -> u32 {
        if let Some(id) = self.heading_cs {
            return id;
        }
        let mut cs = doc
            .doc_info
            .char_shapes
            .get(self.normal_cs as usize)
            .cloned()
            .unwrap_or_default();
        cs.raw_data = None;
        cs.bold = true;
        doc.doc_info.char_shapes.push(cs);
        let id = (doc.doc_info.char_shapes.len() - 1) as u32;
        self.heading_cs = Some(id);
        id
    }

    /// 표·셀이 참조할 실선 테두리 ID(1 기준) — 사방 실선인 항목이 있으면 재사용, 없으면
    /// 덧붙인다.
    fn solid_border_fill(&mut self, doc: &mut crate::model::document::Document) -> u16 {
        if let Some(id) = self.solid_bf {
            return id;
        }
        let existing = doc.doc_info.border_fills.iter().position(|bf| {
            bf.borders
                .iter()
                .all(|b| b.line_type == BorderLineType::Solid && b.width > 0)
        });
        let id = match existing {
            Some(idx) => (idx + 1) as u16,
            None => {
                doc.doc_info.border_fills.push(BorderFill {
                    borders: [BorderLine {
                        line_type: BorderLineType::Solid,
                        width: 1,
                        color: 0,
                    }; 4],
                    ..Default::default()
                });
                doc.doc_info.border_fills.len() as u16
            }
        };
        self.solid_bf = Some(id);
        id
    }
}
