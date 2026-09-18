//! Markdown(부분집합) → [`Block`] 시퀀스 파서.
//!
//! 대화창에서 오간 문서 초안은 거의 언제나 Markdown 꼴이다 — 제목(`#`), 문단, 글머리표,
//! 파이프 표. 에이전트가 이를 `scaffold` JSON 으로 손수 옮기게 하면 호출이 길어지고
//! 오타 표면이 넓어진다. 본 모듈은 그 변환을 서버 안에서 끝낸다.
//!
//! 지원 범위는 [`Block`] 이 왕복 보장하는 세 요소로 **일부러** 좁다:
//!
//! | Markdown | Block |
//! |---|---|
//! | `#`~`#######` 제목 | `Heading { level }` (1~7 클램프) |
//! | 빈 줄로 나뉜 연속 줄 | `Paragraph` (줄은 공백 하나로 잇는다) |
//! | `-`·`*`·`+` 글머리표 | 항목마다 `Paragraph`("• " 접두) |
//! | `1.`·`1)` 번호 항목 | 항목마다 `Paragraph`(번호 표기 보존) |
//! | 파이프 표(`\| a \| b \|`) | `Table { rows }` (구분선 `\|---\|` 행은 버린다) |
//! | ```` ``` ```` 코드 블록 | 줄마다 `Paragraph`(원문 그대로) |
//! | `>` 인용 | 접두 제거 후 `Paragraph` |
//! | `---`·`***` 수평선 | 무시 |
//!
//! 인라인 강조(`**굵게**`·`` `코드` ``·`_기울임_`)는 표식만 벗기고 평문으로 남긴다 —
//! 글자 모양까지 옮기는 것은 후속 축이다. 링크 `[본문](url)` 은 본문만 남긴다.
//! 미지 구문은 실패가 아니라 **평문 문단**으로 흘려보낸다: 입력이 사람의 초안이라
//! 기계 생성 JSON 처럼 fail-fast 할 이득이 없다.

use crate::scaffold::schema::Block;

/// Markdown 텍스트를 [`Block`] 시퀀스로 바꾼다. 빈 입력은 빈 벡터다.
pub fn parse_markdown_blocks(markdown: &str) -> Vec<Block> {
    let lines: Vec<&str> = markdown.lines().map(|l| l.trim_end_matches('\r')).collect();
    let mut blocks: Vec<Block> = Vec::new();
    let mut para_buf: Vec<String> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim();

        // 빈 줄 — 문단 경계.
        if line.is_empty() {
            flush_para(&mut para_buf, &mut blocks);
            i += 1;
            continue;
        }

        // 코드 블록 — 닫는 펜스까지 원문 그대로 한 줄 한 문단.
        if line.starts_with("```") || line.starts_with("~~~") {
            flush_para(&mut para_buf, &mut blocks);
            let fence = &line[..3];
            i += 1;
            while i < lines.len() && !lines[i].trim().starts_with(fence) {
                let code = lines[i].trim_end();
                blocks.push(Block::Paragraph {
                    text: code.to_string(),
                });
                i += 1;
            }
            i += 1; // 닫는 펜스(없으면 EOF)
            continue;
        }

        // 수평선.
        if is_horizontal_rule(line) {
            flush_para(&mut para_buf, &mut blocks);
            i += 1;
            continue;
        }

        // 제목.
        if let Some((level, text)) = parse_heading(line) {
            flush_para(&mut para_buf, &mut blocks);
            blocks.push(Block::Heading {
                level,
                text: strip_inline(text),
            });
            i += 1;
            continue;
        }

        // 파이프 표 — 연속된 `|` 시작 줄.
        if is_table_line(line) {
            flush_para(&mut para_buf, &mut blocks);
            let mut rows: Vec<Vec<String>> = Vec::new();
            while i < lines.len() && is_table_line(lines[i].trim()) {
                let cells = split_table_row(lines[i].trim());
                if !is_separator_row(&cells) {
                    rows.push(cells.iter().map(|c| strip_inline(c)).collect());
                }
                i += 1;
            }
            if !rows.is_empty() {
                blocks.push(Block::Table { rows });
            }
            continue;
        }

        // 글머리표 / 번호 항목 — 항목마다 문단.
        if let Some(item) = parse_bullet(line) {
            flush_para(&mut para_buf, &mut blocks);
            blocks.push(Block::Paragraph {
                text: format!("• {}", strip_inline(item)),
            });
            i += 1;
            continue;
        }
        if let Some((marker, item)) = parse_ordered(line) {
            flush_para(&mut para_buf, &mut blocks);
            blocks.push(Block::Paragraph {
                text: format!("{marker} {}", strip_inline(item)),
            });
            i += 1;
            continue;
        }

        // 인용 — 접두만 벗긴다.
        let body = line
            .strip_prefix('>')
            .map(|s| s.trim_start())
            .unwrap_or(line);
        para_buf.push(strip_inline(body));
        i += 1;
    }
    flush_para(&mut para_buf, &mut blocks);
    blocks
}

fn flush_para(buf: &mut Vec<String>, blocks: &mut Vec<Block>) {
    if buf.is_empty() {
        return;
    }
    let text = buf.join(" ");
    buf.clear();
    let text = text.trim();
    if !text.is_empty() {
        blocks.push(Block::Paragraph {
            text: text.to_string(),
        });
    }
}

fn parse_heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if hashes == 0 || hashes > 7 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with(' ') && !rest.is_empty() {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    Some((hashes as u8, strip_leading_ordinal(text)))
}

/// 제목 앞의 손 번호(`1.`·`1.1`·`2)`·`(3)`)를 벗긴다.
///
/// 개요 수준 문단은 한글이 개요 번호를 **자동으로** 붙인다 — `# 1. 개요` 를 그대로 넣으면
/// 조판 결과가 `1.1. 개요` 가 된다. Markdown 초안의 번호는 개요 수준을 표현한 것이므로
/// 번호는 버리고 수준(`#` 개수)만 남긴다. 연도·수량처럼 뒤에 단위가 붙은 숫자(`2026년`)는
/// 구두점 규칙(`.`·`)` 뒤 공백)에 걸리지 않아 그대로 남는다.
fn strip_leading_ordinal(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut i = 0;
    // `(1)` 꼴.
    if bytes.first() == Some(&b'(') {
        let digits = bytes[1..].iter().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0 && bytes.get(1 + digits) == Some(&b')') {
            let rest = &text[2 + digits..];
            if rest.starts_with(' ') {
                return rest.trim_start();
            }
        }
        return text;
    }
    // `1.` / `1.1` / `1.1.` / `1)` 꼴 — 숫자 그룹과 구두점의 반복.
    let mut groups = 0usize;
    while i < bytes.len() {
        let digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            break;
        }
        groups += 1;
        i += digits;
        match bytes.get(i) {
            Some(b'.') | Some(b')') => i += 1,
            _ => break,
        }
    }
    if groups == 0 {
        return text;
    }
    // 구두점으로 끝났거나(`1.`·`2)`) 점으로 이은 두 그룹 이상(`1.1 매출`)일 때만 번호로
    // 본다 — `3 대안` 처럼 맨숫자 하나는 제목 본문일 수 있어 남긴다.
    let ended_with_punct = matches!(bytes.get(i - 1), Some(b'.') | Some(b')'));
    match text[i..].strip_prefix(' ') {
        Some(rest) if ended_with_punct || groups >= 2 => rest.trim_start(),
        _ => text,
    }
}

fn is_horizontal_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3
        && (compact.chars().all(|c| c == '-')
            || compact.chars().all(|c| c == '*')
            || compact.chars().all(|c| c == '_'))
}

fn is_table_line(line: &str) -> bool {
    line.starts_with('|') && line.len() > 1
}

fn split_table_row(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|');
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    // `\|` 이스케이프는 셀 안의 파이프로 남긴다.
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cur.push('|');
            chars.next();
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

fn is_separator_row(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|c| {
            let t = c.trim();
            !t.is_empty()
                && t.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')
                && t.contains('-')
        })
}

fn parse_bullet(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest.trim());
        }
    }
    None
}

fn parse_ordered(line: &str) -> Option<(&str, &str)> {
    let digits = line.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 4 {
        return None;
    }
    let rest = &line[digits..];
    let after = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some((&line[..digits + 1], after.trim()))
}

/// 인라인 표식을 벗겨 평문으로 만든다 — `**`·`__`·`` ` ``·`~~` 제거, `[본문](url)` → 본문,
/// 낱말 경계의 `*기울임*`·`_기울임_` 제거. 표식이 아닌 `*`·`_`(수식·식별자)는 남긴다.
pub fn strip_inline(text: &str) -> String {
    let mut s = text.replace("**", "").replace("__", "").replace("~~", "");
    s = s.replace('`', "");
    s = strip_links(&s);
    s = strip_paired_marker(&s, '*');
    s = strip_paired_marker(&s, '_');
    s.trim().to_string()
}

fn strip_links(s: &str) -> String {
    // `[text](url)` → `text`. 이미지(`![alt](src)`)는 alt 만 남긴다.
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let start = if chars[i] == '!' && chars.get(i + 1) == Some(&'[') {
            Some(i + 1)
        } else if chars[i] == '[' {
            Some(i)
        } else {
            None
        };
        if let Some(open) = start {
            if let Some(close) = (open + 1..chars.len()).find(|&j| chars[j] == ']') {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(end) = (close + 2..chars.len()).find(|&j| chars[j] == ')') {
                        out.extend(&chars[open + 1..close]);
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn strip_paired_marker(s: &str, marker: char) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == marker {
            let at_open = i == 0 || chars[i - 1].is_whitespace() || is_punct_open(chars[i - 1]);
            let next_is_text = chars
                .get(i + 1)
                .is_some_and(|c| !c.is_whitespace() && *c != marker);
            if at_open && next_is_text {
                let close = (i + 2..chars.len()).find(|&j| {
                    chars[j] == marker
                        && !chars[j - 1].is_whitespace()
                        && chars.get(j + 1).map_or(true, |c| !c.is_alphanumeric())
                });
                if let Some(close) = close {
                    out.extend(&chars[i + 1..close]);
                    i = close + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_punct_open(c: char) -> bool {
    matches!(c, '(' | '[' | '{' | '"' | '\'' | '「' | '（' | '·')
}
