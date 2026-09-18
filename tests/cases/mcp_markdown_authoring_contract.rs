//! [Markdown 저작] 대화 초안 → HWP/HWPX 계약.
//!
//! 세 세션 도구가 이 계약의 주인공이다 — `hwp_new_from_markdown`(초안으로 새 핸들),
//! `hwp_doc_insert_markdown`(열린 핸들에 블록 삽입), `hwp_doc_delete_paragraph`(문단 삭제).
//! 저장은 기존 `hwp_doc_save` 가 맡고, 출력 확장자(.hwp/.hwpx)가 형식을 정한다.
//!
//! 검증은 두 층이다: ① 라이브러리 층 — Markdown 파서가 블록을 어떻게 만드는가,
//! ② 서버 층 — 실제 `mcp-serve` 프로세스와 stdio 로 왕복해 저장본을 **재파싱**했을 때
//! 대화에서 넣은 텍스트가 그대로 있는가.
#![cfg(not(target_arch = "wasm32"))]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use rhwp::scaffold::{parse_markdown_blocks, strip_inline, Block};
use rhwp::wasm_api::HwpDocument;

const HWP5_SAMPLE: &str = "samples/hwp3-sample-hwp5.hwp";

fn sample(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn temp_path(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rhwp-md-authoring-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).expect("임시 디렉터리");
    dir.join(name)
}

fn body_text(doc: &mut HwpDocument) -> String {
    let mut out = String::new();
    for p in 0..doc.page_count() {
        if let Ok(t) = doc.extract_page_text_native(p) {
            out.push_str(&t);
            out.push('\n');
        }
    }
    out
}

// ── ① 라이브러리 층 ────────────────────────────────────────────────────────

#[test]
fn markdown_parser_maps_headings_paragraphs_lists_and_tables() {
    let md = "# 1. 개요\n\n본 문서는 **자동** 생성되었습니다.\n둘째 줄은 같은 문단.\n\n## 1.1 항목\n\n- 첫째\n- 둘째\n\n1. 하나\n2) 둘\n\n| 항목 | 값 |\n|---|---:|\n| 매출 | 100 |\n| 이익 | `20` |\n\n---\n\n> 인용문\n\n```\nlet x = 1;\n```\n";
    let blocks = parse_markdown_blocks(md);
    let mut it = blocks.iter();
    assert!(matches!(it.next(), Some(Block::Heading { level: 1, text }) if text == "개요"));
    assert!(matches!(
        it.next(),
        Some(Block::Paragraph { text }) if text == "본 문서는 자동 생성되었습니다. 둘째 줄은 같은 문단."
    ));
    assert!(matches!(it.next(), Some(Block::Heading { level: 2, text }) if text == "항목"));
    assert!(matches!(it.next(), Some(Block::Paragraph { text }) if text == "• 첫째"));
    assert!(matches!(it.next(), Some(Block::Paragraph { text }) if text == "• 둘째"));
    assert!(matches!(it.next(), Some(Block::Paragraph { text }) if text == "1. 하나"));
    assert!(matches!(it.next(), Some(Block::Paragraph { text }) if text == "2) 둘"));
    match it.next() {
        Some(Block::Table { rows }) => {
            assert_eq!(rows.len(), 3, "구분선 행은 버린다: {rows:?}");
            assert_eq!(rows[0], vec!["항목", "값"]);
            assert_eq!(
                rows[2],
                vec!["이익", "20"],
                "인라인 코드 표식 제거: {rows:?}"
            );
        }
        other => panic!("표 블록이어야 합니다: {other:?}"),
    }
    assert!(matches!(it.next(), Some(Block::Paragraph { text }) if text == "인용문"));
    assert!(matches!(it.next(), Some(Block::Paragraph { text }) if text == "let x = 1;"));
    assert!(
        it.next().is_none(),
        "수평선은 블록을 만들지 않는다: {blocks:?}"
    );
}

#[test]
fn markdown_inline_markers_are_stripped_but_plain_symbols_survive() {
    assert_eq!(
        strip_inline("**굵게** 와 _기울임_ 과 `코드`"),
        "굵게 와 기울임 과 코드"
    );
    assert_eq!(
        strip_inline("[한컴](https://example.com) 참고"),
        "한컴 참고"
    );
    assert_eq!(
        strip_inline("a * b = c"),
        "a * b = c",
        "낱말 사이 별표는 연산자"
    );
    assert_eq!(strip_inline("snake_case_name"), "snake_case_name");
    // 제목 앞 손 번호는 개요 자동 번호와 겹치므로 벗긴다 — 연도·괄호 없는 숫자는 남긴다.
    let heads: Vec<String> = parse_markdown_blocks(
        "# 1. 개요
## 1.1 매출
## 2) 이익
### (3) 비고
# 2026년 계획
# 3
# 3 대안
",
    )
    .into_iter()
    .map(|b| match b {
        Block::Heading { text, .. } => text,
        other => panic!("{other:?}"),
    })
    .collect();
    assert_eq!(
        heads,
        ["개요", "매출", "이익", "비고", "2026년 계획", "3", "3 대안"]
    );
    assert!(parse_markdown_blocks("").is_empty());
    assert!(parse_markdown_blocks("\n\n   \n").is_empty());
}

#[test]
fn insert_blocks_into_parsed_hwp5_document_reuses_or_adds_shapes() {
    let p = sample(HWP5_SAMPLE);
    if !p.exists() {
        eprintln!("샘플 없음 — 건너뜀");
        return;
    }
    let bytes = std::fs::read(&p).unwrap();
    let mut doc = HwpDocument::from_bytes(&bytes).expect("HWP5 파싱");
    let before_ps = doc.document().doc_info.para_shapes.len();
    let before_paras = doc.document().sections[0].paragraphs.len();
    let report = doc
        .insert_markdown_native(
            0,
            None,
            "## 삽입 제목\n\n삽입 본문 문단.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        )
        .expect("삽입");
    assert_eq!(report.first_paragraph, before_paras, "생략 위치는 구역 끝");
    assert_eq!(
        report.inserted_paragraphs, 4,
        "제목+문단+표+표 뒤 빈 문단: {report:?}"
    );
    assert_eq!(
        (
            report.heading_count,
            report.paragraph_count,
            report.table_count
        ),
        (1, 1, 1)
    );
    assert_eq!(report.paragraph_count_after, before_paras + 4);
    let after_ps = doc.document().doc_info.para_shapes.len();
    assert!(
        after_ps >= before_ps,
        "기존 문단 모양은 지우지 않고 필요한 것만 덧붙인다"
    );
    let heading = &doc.document().sections[0].paragraphs[before_paras];
    let ps = &doc.document().doc_info.para_shapes[heading.para_shape_id as usize];
    assert_eq!(ps.head_type, rhwp::model::style::HeadType::Outline);
    assert_eq!(ps.para_level, 1, "## 은 개요 2수준(para_level=1)");
    assert!(
        ps.raw_data.is_none(),
        "덧붙인 모양은 원본 바이트 지름길이 없어야 모델 값으로 직렬화된다"
    );

    // 같은 수준 제목을 또 넣으면 모양을 재사용한다.
    let ps_count = doc.document().doc_info.para_shapes.len();
    doc.insert_markdown_native(0, None, "## 두 번째 제목")
        .expect("재삽입");
    assert_eq!(doc.document().doc_info.para_shapes.len(), ps_count);

    // 범위 오류는 실패다 — 조용히 끝에 붙이지 않는다.
    let count = doc.document().sections[0].paragraphs.len();
    assert!(doc.insert_markdown_native(0, Some(count + 1), "x").is_err());
    assert!(doc.insert_markdown_native(9, None, "x").is_err());

    // HWP5 로 다시 쓰고 재파싱해도 텍스트가 남는다.
    let out = doc.export_hwp_with_adapter_snapshot().expect("HWP5 직렬화");
    let mut reparsed = HwpDocument::from_bytes(&out).expect("재파싱");
    let text = body_text(&mut reparsed);
    for needle in ["삽입 제목", "삽입 본문 문단.", "두 번째 제목"] {
        assert!(text.contains(needle), "{needle} 가 저장본에 없다");
    }
}

// ── ② 서버 층 ──────────────────────────────────────────────────────────────

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Server {
    fn start(extra: &[&str]) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rhwp"))
            .arg("mcp-serve")
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("rhwp mcp-serve 실행 실패");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut s = Server {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let r = s.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "md-authoring-test", "version": "0"}
            }),
        );
        assert!(r["result"]["serverInfo"]["name"].is_string(), "{r}");
        let msg = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        writeln!(s.stdin, "{msg}").unwrap();
        s.stdin.flush().unwrap();
        s
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").expect("요청 쓰기 실패");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line).expect("응답 읽기 실패");
            assert!(n > 0, "서버가 응답 없이 종료했습니다 (method={method})");
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(line.trim())
                .unwrap_or_else(|e| panic!("stdout 이 순수 JSON-RPC 가 아닙니다 ({e}): {line}"));
            if v.get("id").and_then(|i| i.as_i64()) == Some(id) {
                return v;
            }
        }
    }

    /// tools/call → (isError, content[0].text 를 JSON 으로 파싱한 값 또는 원문).
    fn call(&mut self, name: &str, args: serde_json::Value) -> (bool, serde_json::Value) {
        let r = self.request(
            "tools/call",
            serde_json::json!({"name": name, "arguments": args}),
        );
        let result = &r["result"];
        let is_error = result["isError"].as_bool().unwrap_or_else(|| panic!("{r}"));
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        let body =
            serde_json::from_str(text).unwrap_or(serde_json::Value::String(text.to_string()));
        (is_error, body)
    }

    fn call_ok(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        let (err, body) = self.call(name, args);
        assert!(!err, "{name} 이 isError 를 보고했습니다: {body}");
        body
    }

    fn tool_names(&mut self) -> Vec<String> {
        let r = self.request("tools/list", serde_json::json!({}));
        r["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|t| t["name"].as_str().map(String::from))
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const DRAFT: &str = "# 2026년 상반기 사업 보고\n\n본 보고서는 대화창에서 작성한 초안을 그대로 옮긴 것입니다.\n\n## 1. 실적 요약\n\n- 매출 120억 원 (전년 대비 +12%)\n- 영업이익 18억 원\n\n| 구분 | 상반기 | 하반기 계획 |\n|---|---|---|\n| 매출 | 120 | 140 |\n| 영업이익 | 18 | 22 |\n\n## 2. 향후 계획\n\n하반기에는 **신규 채널** 확대에 집중합니다.\n";

#[test]
fn new_from_markdown_then_save_as_hwp_and_hwpx_round_trips_text() {
    let mut s = Server::start(&[]);
    let opened = s.call_ok(
        "hwp_new_from_markdown",
        serde_json::json!({ "markdown": DRAFT, "title": "사업 보고서" }),
    );
    let doc_id = opened["docId"].as_str().expect("docId").to_string();
    assert_eq!(opened["source"], "markdown");
    assert_eq!(opened["headingCount"], 3, "{opened}");
    assert_eq!(opened["tableCount"], 1, "{opened}");
    assert!(opened["pageCount"].as_u64().unwrap_or(0) >= 1, "{opened}");
    assert_eq!(opened["nextCall"]["name"], "hwp_doc_save", "{opened}");

    // 핸들은 기존 조회 도구로 곧바로 읽힌다.
    let text = s.call_ok("hwp_doc_text", serde_json::json!({ "docId": doc_id }));
    let joined = text.to_string();
    assert!(joined.contains("사업 보고서"), "제목 문단: {joined}");
    assert!(
        joined.contains("신규 채널"),
        "인라인 강조는 평문으로: {joined}"
    );
    let structure = s.call_ok("hwp_doc_structure", serde_json::json!({ "docId": doc_id }));
    assert!(
        structure.to_string().contains("실적 요약"),
        "# 제목은 개요 노드로 보인다: {structure}"
    );

    // 같은 핸들을 .hwpx 와 .hwp 로 저장 — 확장자가 형식을 정한다.
    let hwpx = temp_path("draft.hwpx");
    let saved = s.call_ok(
        "hwp_doc_save",
        serde_json::json!({ "docId": doc_id, "output": hwpx.to_string_lossy(), "verify": true }),
    );
    assert_eq!(saved["outputFormat"], "hwpx", "{saved}");
    assert_eq!(
        saved["verify"]["identical"], true,
        "저장본 재파싱 자기검증: {saved}"
    );

    let hwp = temp_path("draft.hwp");
    let saved = s.call_ok(
        "hwp_doc_save",
        serde_json::json!({ "docId": doc_id, "output": hwp.to_string_lossy() }),
    );
    assert_eq!(saved["outputFormat"], "hwp5", "{saved}");
    assert!(saved["bytes"].as_u64().unwrap_or(0) > 0);

    for path in [&hwpx, &hwp] {
        let bytes = std::fs::read(path).expect("저장 파일");
        let mut reparsed = HwpDocument::from_bytes(&bytes)
            .unwrap_or_else(|e| panic!("{} 재파싱 실패: {e}", path.display()));
        let body = body_text(&mut reparsed);
        for needle in [
            "사업 보고서",
            "2026년 상반기 사업 보고",
            "본 보고서는 대화창에서 작성한 초안을 그대로 옮긴 것입니다.",
            "• 매출 120억 원",
            "영업이익",
            "하반기 계획",
            "신규 채널",
        ] {
            assert!(
                body.contains(needle),
                "{} 에 '{needle}' 이 없다:\n{body}",
                path.display()
            );
        }
    }
    let closed = s.call_ok("hwp_close", serde_json::json!({ "docId": doc_id }));
    assert_eq!(closed["closed"], true);
}

#[test]
fn insert_markdown_into_opened_hwp5_at_end_after_text_and_delete_paragraph() {
    let p = sample(HWP5_SAMPLE);
    if !p.exists() {
        eprintln!("샘플 없음 — 건너뜀");
        return;
    }
    let mut s = Server::start(&[]);
    let opened = s.call_ok(
        "hwp_open",
        serde_json::json!({ "path": p.to_str().unwrap() }),
    );
    let doc_id = opened["docId"].as_str().unwrap().to_string();

    // ① 생략 → 마지막 구역 끝.
    let r = s.call_ok(
        "hwp_doc_insert_markdown",
        serde_json::json!({ "docId": doc_id, "markdown": "## 대화에서 추가한 절\n\n추가 본문 첫 문단." }),
    );
    assert_eq!(r["insertedParagraphs"], 2, "{r}");
    assert_eq!(r["headingCount"], 1, "{r}");
    let first = r["firstParagraph"].as_u64().unwrap() as usize;
    assert!(r["changedPages"].is_array(), "재조판 뒤 쪽 번호: {r}");

    // ② afterText → 그 문단 바로 뒤.
    let r = s.call_ok(
        "hwp_doc_insert_markdown",
        serde_json::json!({ "docId": doc_id, "afterText": "추가 본문 첫 문단", "markdown": "afterText 로 끼운 문단." }),
    );
    assert_eq!(
        r["firstParagraph"].as_u64().unwrap() as usize,
        first + 2,
        "{r}"
    );

    // ③ at → 인덱스 앞.
    let r = s.call_ok(
        "hwp_doc_insert_markdown",
        serde_json::json!({ "docId": doc_id, "section": 0, "at": first, "markdown": "at 으로 끼운 문단." }),
    );
    assert_eq!(r["firstParagraph"].as_u64().unwrap() as usize, first, "{r}");
    let count_after = r["paragraphCountAfter"].as_u64().unwrap();

    // 검색 주소와 삽입 주소가 같은 좌표계다.
    let found = s.call_ok(
        "hwp_doc_search",
        serde_json::json!({ "docId": doc_id, "query": "at 으로 끼운 문단" }),
    );
    let m = &found["matches"][0];
    assert_eq!(m["paragraph"].as_u64().unwrap() as usize, first, "{found}");

    // ④ 삭제 — 같은 주소로 지운다.
    let r = s.call_ok(
        "hwp_doc_delete_paragraph",
        serde_json::json!({ "docId": doc_id, "section": 0, "paragraph": first }),
    );
    assert_eq!(r["deleted"], true, "{r}");
    assert_eq!(
        r["paragraphCountAfter"].as_u64().unwrap(),
        count_after - 1,
        "{r}"
    );
    assert!(
        r["removedTextPreview"]
            .as_str()
            .unwrap_or("")
            .starts_with("at 으로"),
        "{r}"
    );
    let found = s.call_ok(
        "hwp_doc_search",
        serde_json::json!({ "docId": doc_id, "query": "at 으로 끼운 문단" }),
    );
    assert_eq!(
        found["totalMatchCount"], 0,
        "지운 문단은 검색되지 않는다: {found}"
    );

    // ⑤ 오류 층 — 위치 인자 충돌·미발견 afterText·범위 밖 삭제는 isError.
    let (err, body) = s.call(
        "hwp_doc_insert_markdown",
        serde_json::json!({ "docId": doc_id, "at": 0, "afterText": "x", "markdown": "y" }),
    );
    assert!(err, "at+afterText 동시 지정은 거부: {body}");
    let (err, body) = s.call(
        "hwp_doc_insert_markdown",
        serde_json::json!({ "docId": doc_id, "afterText": "존재하지않는문구zzz", "markdown": "y" }),
    );
    assert!(err, "{body}");
    assert_eq!(
        body["nextCall"]["name"], "hwp_doc_search",
        "교정 호출 동봉: {body}"
    );
    let (err, _) = s.call(
        "hwp_doc_delete_paragraph",
        serde_json::json!({ "docId": doc_id, "paragraph": 100000 }),
    );
    assert!(err);
    let (err, body) = s.call(
        "hwp_doc_insert_markdown",
        serde_json::json!({ "docId": "doc-999", "markdown": "y" }),
    );
    assert!(err);
    assert_eq!(body["nextCall"]["name"], "hwp_open", "{body}");

    // ⑥ 저장 → 재파싱: 대화에서 넣은 텍스트가 HWP5 저장본에 남고 지운 문단은 없다.
    let out = temp_path("edited.hwp");
    let saved = s.call_ok(
        "hwp_doc_save",
        serde_json::json!({ "docId": doc_id, "output": out.to_string_lossy() }),
    );
    assert_eq!(saved["outputFormat"], "hwp5", "{saved}");
    let bytes = std::fs::read(&out).unwrap();
    let mut reparsed = HwpDocument::from_bytes(&bytes).expect("저장본 재파싱");
    let body = body_text(&mut reparsed);
    assert!(body.contains("대화에서 추가한 절"), "{body}");
    assert!(body.contains("afterText 로 끼운 문단."), "{body}");
    assert!(!body.contains("at 으로 끼운 문단"), "{body}");
}

#[test]
fn authoring_tools_are_listed_with_annotations_and_gated_by_profile() {
    let mut s = Server::start(&[]);
    let r = s.request("tools/list", serde_json::json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    for name in [
        "hwp_new_from_markdown",
        "hwp_doc_insert_markdown",
        "hwp_doc_delete_paragraph",
    ] {
        let t = tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} 미등재"));
        assert!(t["inputSchema"]["required"].is_array(), "{t}");
        assert_eq!(t["annotations"]["readOnlyHint"], false, "{t}");
        assert_eq!(t["annotations"]["destructiveHint"], false, "{t}");
        assert_eq!(t["annotations"]["idempotentHint"], false, "{t}");
    }
    drop(s);

    // 콘텐츠제작 프로필은 저작 세션을 열고, 데이터분석은 세션을 아예 열지 않는다.
    let mut content = Server::start(&["--profile", "콘텐츠제작"]);
    let names = content.tool_names();
    for name in [
        "hwp_new_from_markdown",
        "hwp_doc_insert_markdown",
        "hwp_doc_save",
    ] {
        assert!(names.contains(&name.to_string()), "{name}: {names:?}");
    }
    assert!(
        !names.contains(&"hwp_doc_fill_fields".to_string()),
        "{names:?}"
    );
    drop(content);

    let mut data = Server::start(&["--profile", "데이터분석"]);
    let names = data.tool_names();
    assert!(
        !names.contains(&"hwp_new_from_markdown".to_string()),
        "{names:?}"
    );
    let (err, _) = data.call(
        "hwp_new_from_markdown",
        serde_json::json!({ "markdown": "x" }),
    );
    assert!(err, "목록에서 뺀 도구는 호출로도 우회할 수 없다");
}

#[test]
fn new_from_markdown_rejects_bad_page_size_and_accepts_empty_draft() {
    let mut s = Server::start(&[]);
    let (err, body) = s.call(
        "hwp_new_from_markdown",
        serde_json::json!({ "markdown": "x", "pageWidthMm": -1 }),
    );
    assert!(err, "{body}");
    let (err, body) = s.call("hwp_new_from_markdown", serde_json::json!({}));
    assert!(err, "markdown 누락은 거부: {body}");
    let opened = s.call_ok(
        "hwp_new_from_markdown",
        serde_json::json!({ "markdown": "", "pageWidthMm": 297, "pageHeightMm": 210 }),
    );
    assert_eq!(opened["blockCount"], 0, "{opened}");
    assert!(
        opened["pageCount"].as_u64().unwrap_or(0) >= 1,
        "빈 문서도 한 쪽: {opened}"
    );
    let info = s.call_ok(
        "hwp_doc_info",
        serde_json::json!({ "docId": opened["docId"] }),
    );
    assert!(info.is_object(), "{info}");
}
