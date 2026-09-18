---
kind: guide
status: active
canonical: mydocs/manual/mcp_integration_guide.md
last_verified: 2026-08-24
---

# MCP 통합 가이드 — AI 에이전트 호스트에 rhwp 붙이기

rhwp 를 MCP(Model Context Protocol) 도구로 소비하는 **두 경로**를 공식화한다.
반대 방향(한컴 앱을 원격 구동하는 내부 검증용 클라이언트)은
[HWP 2024 MCP 사용법](mcp_hwp2024Convert_usage.md)이 다루며 본 문서와 무관하다. 2022 이하 저장본은
통합 Windows service의 engine `2020`, 2024 저장본은 engine `2024`를 선택한다. 원격 변환 service는
rhwp maintainer, collaborator 또는 MCP 관리자가 별도로 인증한 사용자만 사용한다.

| 경로 | 무엇 | 언제 |
|---|---|---|
| ① 매니페스트 소비 | `capabilities --mcp` 출력(JSON)을 읽어 호스트가 직접 CLI 를 조립·실행 | MCP 가 아닌 함수콜 클라이언트, 자체 러너, 감사 가능한 배선이 필요할 때 |
| ② 표준 서버 | `rhwp mcp-serve` — stdio JSON-RPC 로 MCP 프로토콜을 직접 처리 (#3140, #3571) | Claude Code 등 MCP 호스트에 설정 한 줄로 붙일 때, 세션(재파싱 회피)이 필요할 때 |

두 경로의 도구 정의는 **단일 출처**(`src/main.rs` 의 `mcp_tool_definitions()`)다.
①에서 보이는 선언과 ②가 실행하는 목록은 같은 코드에서 나오며, 계약 테스트
(`tests/mcp_server_contract.rs::tools_list_matches_capabilities_manifest`)가 어긋남을 잡는다.

## 경로 ② — `mcp-serve` 표준 서버

### 호스트 등록

```jsonc
// Claude Code — 프로젝트 루트 .mcp.json
{ "mcpServers": { "rhwp": { "command": "rhwp", "args": ["mcp-serve"] } } }
```

`rhwp` 가 PATH 에 없으면 `command` 에 절대 경로를 쓴다. 전송은 stdio 뿐이므로
네트워크 포트·인증 설정이 없다. 서버는 stdin EOF 에서 종료한다. 저장소 루트
[`.mcp.json`](../../.mcp.json)이 Claude Code 용으로 이미 rhwp 를 붙여 둔다.

**다른 호스트에 붙이려면** — Claude Desktop·Cursor·Cline·Continue·Windsurf·
VS Code·Zed·Goose·Gemini CLI 등 18개 호스트별 설정은
[`mcp_attach_kit.md`](mcp_attach_kit.md)에 모아 두었다.

### 프로토콜 표면

| 메서드 | 응답 |
|---|---|
| `initialize` | `protocolVersion`(클라이언트 제안 에코), `capabilities.tools`, `serverInfo{name:"rhwp",version}` |
| `notifications/initialized` | (알림 — 무응답) |
| `ping` | `{}` |
| `tools/list` | 선언 도구 전부 + 세션 도구. 각 항목은 MCP 필수 3종(`name`/`description`/`inputSchema`) |
| `tools/call` | `content[0].text` 에 CLI 와 동일한 JSON 봉투. JSON 이면 `structuredContent` 로도 병행 제공 |

지원하지 않는 메서드는 JSON-RPC `-32601`, 파싱 불가 입력은 `-32700`,
`params` 구조 오류는 `-32602` 다.

### 오류 의미론 — 세 층을 혼동하지 않기

| 층 | 신호 | 예 |
|---|---|---|
| JSON-RPC 오류 | `error{code,message}` | 알 수 없는 메서드(-32601), `params.name` 누락(-32602) |
| 도구 실행 실패 | `result.isError: true` + 사유 텍스트 | 없는 파일, 닫힌 핸들 재사용, 알 수 없는 도구 이름 |
| 도구가 성공적으로 전한 "부정적 결과" | `isError: false`, 봉투 안 필드 | `ir-diff` 의 `identical:false`, `fill-fields` 의 `notFound` |

셋째 층이 중요하다: **차이 발견·부분 실패는 오류가 아니라 데이터다.**
`hwp_ir_diff` 가 차이를 찾으면(CLI 였다면 exit 3) 서버는 `isError:false` 로
`{"identical":false,"diffCount":…}` 를 그대로 돌려준다. 에이전트는 봉투 필드로
판정해야 하며 `isError` 만 보고 "검증 통과"로 오독하면 안 된다.

### 세션 도구 — 재파싱 없는 반복 조회 (서버 전용)

CLI 는 프로세스마다 문서를 다시 파싱한다. 대형 문서를 여러 번 조회할 때는
서버 프로세스가 살아 있는 동안 핸들을 잡아둔다:

```jsonc
→ {"method":"tools/call","params":{"name":"hwp_open","arguments":{"path":"편람.hwp","password":"<보호 문서일 때만>"}}}
← {"docId":"doc-1","source":"편람.hwp","pageCount":393}

→ {"name":"hwp_doc_text","arguments":{"docId":"doc-1","page":41}}   // 재파싱 없음
→ {"name":"hwp_doc_text","arguments":{"docId":"doc-1","page":42}}   // 재파싱 없음

→ {"name":"hwp_close","arguments":{"docId":"doc-1"}}
← {"closed":true}
```

- 핸들은 서버 프로세스 수명과 같다 — 서버가 내려가면 전부 사라진다(영속 아님).
- 닫힌/모르는 `docId` 사용은 `isError:true` ("열려 있지 않은 핸들")다.
- 암호 HWP5·압축 HWP3·ODF 암호 HWPX는 선택 `password`로 연다. `password`는
  `writeOnly` 입력이며 rhwp는 응답·오류·세션 상태에 값을 넣지 않는다. 다만 MCP 호스트의
  대화 기록·telemetry가 도구 인자를 보관할 수 있으므로 신뢰된 로컬 호스트에서만 사용한다.
- 세션 편집은 `hwp_doc_replace_text`/`hwp_doc_set_cell`/`hwp_doc_fill_fields` 로 IR 에
  누적하고 `hwp_doc_save` 가 한 번 기록한다. 대화 초안을 문단으로 옮기는 Markdown 저작
  축은 아래 절을 따른다.

### Markdown 저작 — 대화 초안을 HWP/HWPX 로

에이전트가 대화창에서 만든 초안은 Markdown 이다. 그것을 `scaffold` JSON 으로 손수 옮기지
않고 서버가 블록(제목·문단·글머리표·번호 항목·파이프 표)으로 풀어 문서에 넣는다.
세 도구가 세션 전용으로 더해졌고, 저장은 기존 `hwp_doc_save` 하나뿐이다 — **output 의
확장자가 형식을 정한다**(`.hwp` → HWP5, `.hwpx` → HWPX).

| 도구 | 무엇 | 위치 |
|---|---|---|
| `hwp_new_from_markdown` | `markdown`(+선택 `title`·`font`·`pageWidthMm`·`pageHeightMm`)으로 **새 핸들**을 연다 | — |
| `hwp_doc_insert_markdown` | 열린 핸들에 Markdown 블록을 문단으로 끼운다 | 생략=마지막 구역 끝 / `at`=문단 인덱스 **앞** / `afterText`=첫 매치 문단 **뒤** (`at`·`afterText` 동시 지정은 거부) |
| `hwp_doc_delete_paragraph` | 문단 하나를 지운다 (`section`/`paragraph`, 0 기준) | 여러 개는 큰 인덱스부터 |

새 문서 흐름:

```jsonc
→ {"name":"hwp_new_from_markdown","arguments":{"title":"사업 보고서","markdown":"# 1. 개요

본문…

| 구분 | 값 |
|---|---|
| 매출 | 120 |"}}
← {"docId":"doc-1","pageCount":1,"headingCount":1,"tableCount":1,"nextCall":{"name":"hwp_doc_save",…}}
→ {"name":"hwp_doc_text","arguments":{"docId":"doc-1"}}                      // 눈으로 확인
→ {"name":"hwp_doc_save","arguments":{"docId":"doc-1","output":"보고서.hwp","verify":true}}
← {"outputFormat":"hwp5","bytes":…,"verify":{"identical":true,"diffCount":0}}
→ {"name":"hwp_close","arguments":{"docId":"doc-1"}}
```

기존 문서 수정 흐름:

```jsonc
→ {"name":"hwp_open","arguments":{"path":"원본.hwp"}}                         // docId
→ {"name":"hwp_doc_insert_markdown","arguments":{"docId":"doc-1","afterText":"3. 추진 계획","markdown":"- 대화에서 정한 항목
- 둘째 항목"}}
← {"firstParagraph":41,"insertedParagraphs":2,"changedPages":[3]}
→ {"name":"hwp_doc_render_page","arguments":{"docId":"doc-1","page":3,…}}    // 바뀐 쪽만 눈검증
→ {"name":"hwp_doc_save","arguments":{"docId":"doc-1","output":"수정본.hwp"}}
```

- 지원 구문은 `src/scaffold/markdown.rs` 의 표가 단일 출처다. 인라인 강조(`**`·`` ` ``)는
  표식만 벗겨 평문으로 들어가고, 미지 구문은 실패 대신 평문 문단이 된다.
- 새 문단은 삽입 지점 **앞 문단의 서식을 상속**한다. `#` 제목은 개요 수준 문단
  (`hwp_doc_structure` 가 개요로 인식)이고, 표는 실선 테두리다. 문서에 없는 모양은
  `doc_info` 에 덧붙일 뿐 기존 항목은 고치지 않는다.
- `hwp_new_from_markdown` 핸들은 원본 형식이 HWPX 로 기억되므로 `.hwp` 로 저장하면
  `hwp_convert_hwp5` 와 같은 어댑터 경로를 탄다. 저장본 자기검증은 `verify:true`.
- 세 도구 모두 `idempotentHint:false` 다 — 같은 인자를 다시 보내면 한 번 더 끼우거나
  다음 문단을 지운다. `hwp_ws_journal` 에는 변이로 기록된다.
- 계약: `tests/cases/mcp_markdown_authoring_contract.rs`.

### 무상태 도구는 CLI 계약의 얇은 껍데기

세션 도구를 제외한 문서 처리 도구는 선언의 `cli.args` 배선을 그대로 해석해 rhwp 자신을
서브프로세스로 실행한다. 따라서 **CLI 문서가 곧 도구 문서다**: 봉투 필드는
[CLI 명령어 매뉴얼](cli_commands.md)의 각 `--json` 절, 활용 패턴은
[CLI JSON 파이프라인 가이드](cli_json_pipeline_guide.md)와
[에이전트 실무 대체 예제집](agent_task_playbook.md)을 그대로 적용한다.

암호 입력을 지원하는 선언에는 `inputSchema.properties.password`와
`cli.passwordStdin` 메타가 있다. `password`를 주면 서버는 값 자체를 `cli.args`에 넣지 않고
자식 프로세스의 `--password-stdin` 첫 줄로만 전달한다. `hwp_batch`와
`hwp_batch_search`는 stdin을 경로 목록에 사용하므로 password를 지원하지 않는다.

## 경로 ① — 매니페스트 직접 소비

MCP 호스트가 아닌 클라이언트(자체 함수콜 러너, 감사 필요 환경)는 선언을 직접 읽는다:

```bash
rhwp capabilities --mcp | jq -c '.tools[] | {name, cli: .cli.args, required: .inputSchema.required}'
```

조립 규칙(`invocation.note` 에도 자기서술된다):

1. `cli.args` 배열의 `{키}` 자리표시자를 `inputSchema` 의 같은 이름 입력값으로 치환한다.
   문자열이 아닌 값(객체·숫자)은 JSON 직렬화 문자열로 넣는다 — `hwp_fill_fields` 의
   `{data}` 가 `--data '{"필드":"값"}'` 이 되는 식이다.
2. `invocation.stdinTools` 에 열거된 도구(`hwp_batch`·`hwp_batch_search`)는 `paths`
   배열을 **stdin 한 줄 하나씩**으로 흘려 넣는다.
3. 도구 선언의 `cli.passwordStdin`이 있으면 `password`를 `--password-stdin` 첫 줄로
   전달한다. 이 값은 `cli.args` 자리표시자로 치환하지 않는다.
4. stdout 은 순수 JSON(배치는 NDJSON), 진단은 stderr, 성공 판정은
   [#2707 종료 코드](cli_commands.md#종료-코드-2707)다.

### 종료 코드 ↔ 서버 의미론 대응

| CLI exit | 뜻 | `mcp-serve` 에서는 |
|---:|---|---|
| 0 | 성공 | `isError:false` |
| 1 | 런타임 실패 | stdout 이 비면 `isError:true`(stderr 동봉). 배치 부분 실패처럼 stdout 에 NDJSON 이 있으면 결과로 전달 |
| 2 | 사용법 오류 | `isError:true` — 호출 조립 버그이므로 에이전트는 재시도 대신 인자를 고쳐야 한다 |
| 3 | `ir-diff` 차이 검출 | `isError:false` + `identical:false` (위 "부정적 결과" 층) |

## 도구 선택 지도

| 하려는 일 | 도구 |
|---|---|
| 문서 규모·형식 파악 | `hwp_info` |
| 본문 읽기 (1회) | `hwp_export_text` / (반복) `hwp_open`→`hwp_doc_text` |
| "어느 쪽에 있나" | `hwp_search` (페이지·셀 주소 동봉) |
| 조문·개요 구조 | (1회) `hwp_export_structure` / (반복) `hwp_open`→`hwp_doc_structure` |
| 날짜·금액·수량 추출 | (1회) `hwp_extract_data` / (반복) `hwp_open`→`hwp_doc_extract_data` |
| 표 격자(병합 보존) | (1회) `hwp_export_tables` / (반복) `hwp_open`→`hwp_doc_tables` |
| 누름틀 조사 → 채우기 | `hwp_fields` → `hwp_fill_fields` |
| 표 좌표로 값 쓰기 | `hwp_set_cell` |
| 문구 일괄 치환 | `hwp_replace_text` |
| 대화 초안(Markdown)으로 새 문서 | `hwp_new_from_markdown` → `hwp_doc_save`(`.hwp`/`.hwpx`) |
| 기존 문서에 Markdown 내용 끼우기·문단 삭제 | `hwp_open` → `hwp_doc_insert_markdown` / `hwp_doc_delete_paragraph` → `hwp_doc_save` |
| 시각 확인(VLM) | `hwp_export_svg` |
| 변환·편집 무손실 검증 | `hwp_ir_diff` |
| 아카이브 대량 스윕 | `hwp_batch` / `hwp_batch_search` |

편집 3종의 심화 의미론(반복 필드 순번, 병합 셀, overflow 보고)은
[CLI 명령어 매뉴얼](cli_commands.md)의 `edit` 각 절을 따른다(전용 심화 가이드는 #3574).

## 검증

- 서버 계약: `cargo test --release --test mcp_server_contract` (6건 — 핸드셰이크,
  선언-서버 드리프트 가드, 실호출, 세션 왕복, -32601, 미지 도구 isError)
- 선언 계약: `cargo test --release --test cli_json_contract` (드리프트 가드 ①②③ 포함)
