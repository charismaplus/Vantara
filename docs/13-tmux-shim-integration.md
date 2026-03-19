# tmux Shim Integration

## 1. 목표

Claude/Codex 계열 CLI가 실행 중 `tmux` 명령을 호출하면 실제 tmux 없이도 Workspace Terminal의 탭/패널 모델로 자연스럽게 연결되도록 한다.

핵심 원칙:

- tmux 명령 형태는 유지한다.
- 처리 범위는 caller session이 속한 **현재 탭**으로 제한한다.
- `new-window`는 같은 프로젝트의 새 탭 생성으로 매핑한다.
- 통신은 `127.0.0.1 + session token` 조합으로만 허용한다.

## 2. 구성 요소

### 2.1 앱 백엔드 서비스

- `main.rs`에 `TmuxShimState`와 HTTP 처리 루틴을 둔다.
- 앱 시작 시 `start_tmux_server(...)`가 `127.0.0.1:0`(ephemeral port)로 서버를 올린다.
- 세션별 토큰(`WORKSPACE_TERMINAL_TMUX_TOKEN`)을 `TmuxTokenContext`로 관리한다.

### 2.2 shim 바이너리

- `apps/desktop/src-tauri/src/bin/tmux.rs`를 `tmux.exe`로 빌드한다.
- shim은 `WORKSPACE_TERMINAL_TMUX_URL`, `WORKSPACE_TERMINAL_TMUX_TOKEN`이 있으면 로컬 HTTP API를 호출한다.
- 위 env가 없으면 shim은 PATH에서 실제 tmux를 찾아 fallback 실행한다.
- release portable에서는 이 바이너리를 앱 EXE 안에 embed하고, shim-enabled 세션 시작 시 `%TEMP%` 아래로 자동 추출한다.

### 2.3 세션 환경 주입

`launchProfile != terminal` 세션에서만 아래 env를 주입한다.

- `TMUX=workspace-terminal-shim,<port>,<paneId>`
- `WORKSPACE_TERMINAL_TMUX_URL=http://127.0.0.1:<port>`
- `WORKSPACE_TERMINAL_TMUX_TOKEN=<token>`
- `WORKSPACE_TERMINAL_PROJECT_ID=<projectId>`
- `WORKSPACE_TERMINAL_TAB_ID=<tabId>`
- `WORKSPACE_TERMINAL_PANE_ID=<paneId>`
- `WORKSPACE_TERMINAL_SESSION_ID=<sessionId>`
- `PATH` 맨 앞에 extracted temp shim 디렉터리 prepend

embedded shim이 없는 dev/debug 빌드에서는 기존 sibling `shim/tmux.exe` fallback을 허용한다.

## 3. 명령 지원 범위

### 3.1 핵심 명령

- `split-window`, `splitw`
- `new-window`, `neww`
- `send-keys`, `send`
- `list-panes`, `lsp`
- `kill-pane`, `killp`
- `display-message`, `display`
- `has-session`, `has`

### 3.2 no-op 성공 처리

- `select-pane`, `selectp`
- `resize-pane`, `resizep`
- `set-option`, `set`
- `set-window-option`, `setw`
- `bind-key`, `bind`
- `unbind-key`, `unbind`
- `set-environment`, `setenv`
- `source-file`, `source`

### 3.3 미지원 명령

- stderr에 `unsupported command`를 출력하고 non-zero로 종료한다.

## 4. 매핑 규칙

### 4.1 split-window

- `-h` -> 좌우 분할(`horizontal`)
- `-v` -> 위아래 분할(`vertical`)
- 방향 옵션이 없으면 fallback 정책:
  - 부모 split 축을 우선 확인해 반대 축으로 분할(동일 축 과분할 방지)
  - 부모 정보가 없으면 `horizontal` 기본
- `command`가 없으면 caller CLI를 복제하지 않고 새 shell terminal을 연다.
- 이 shell session에도 동일한 shim env를 주입하므로, 이후 `send-keys`나 중첩 tmux 호출이 계속 동작한다.
- 새 pane 메타:
  - `createdBy=ai`
  - `sourcePaneId=<caller/target pane>`
- `command`가 있으면 `cmd /C <command>`로 실행
- `command`가 없으면 `launchProfile`은 유지하고 실행 프로그램만 shell로 둔다.

### 4.2 new-window

- 같은 프로젝트 내 새 탭 생성
- 루트 pane은 `createdBy=ai`
- 탭은 백그라운드 생성(활성 탭 강제 변경 없음)
- 실행 규칙은 split-window와 동일(`command` 없으면 shell terminal 시작)

### 4.3 send-keys

- 기본 target은 caller pane
- `-t` target은 현재 탭 범위에서만 해석
- pane id(`%...`), 내부 id, pane index를 지원한다.
- 특수 키는 shim에서 텍스트/제어문자로 변환해 `write_input`으로 전달한다.

### 4.4 list-panes

- caller 탭의 pane만 반환
- 지원 포맷 변수:
  - `#{pane_id}` (`%<paneId>` 형태)
  - `#{pane_index}`
  - `#{pane_title}`
  - `#{pane_current_command}`
  - `#{window_index}`
  - `#{session_name}`
  - `#{window_id}`

### 4.5 kill-pane

- target pane 닫기
- pane close 시 해당 pane의 세션도 종료한다.

### 4.6 display-message / has-session

- `display-message -p`는 caller 기반 포맷 치환 결과를 반환한다.
- `has-session`은 caller context가 살아 있으면 성공, 아니면 실패한다.

## 5. 보안/범위 제한

- 모든 shim API 요청은 Bearer token 필수
- token 미존재/불일치 시 즉시 401 반환
- target 해석은 caller tab 범위를 벗어나지 않는다.
- 다른 탭/프로젝트 pane 조작 요청은 실패한다.

## 6. 타입/계약

### 6.1 contracts

- `LaunchProfile = "terminal" | "claude" | "claudeUnsafe" | "codex" | "codexFullAuto"`
- `TerminalSession`:
  - `launchProfile`
  - `tmuxShimEnabled`

### 6.2 backend model/DB

- `sessions.launch_profile TEXT`
- `sessions.tmux_shim_enabled INTEGER`
- 세션 upsert/list에서 필드 일관 유지

## 7. 포터블 배포

- 포터블 기준 경로:
  - `target/release/bundle/portable/Workspace Terminal Portable/Workspace Terminal Portable.exe`
- `scripts/refresh-portable.ps1`:
  - `tmux.exe`를 먼저 빌드
  - 그 경로를 `WORKSPACE_TERMINAL_EMBED_TMUX_PATH`로 넘겨 앱 EXE 빌드
  - 포터블 폴더에는 app EXE만 복사
  - SHA256 동등성 검증
  - 실행 중 파일 잠금 시 실패 처리

런타임 helper 경로:

- `%TEMP%\WorkspaceTerminal\<embedded-hash>\tmux.exe`

## 8. 검증 체크리스트

- Claude/Codex 런처 세션에서 `tmux split-window -h/-v` 동작
- `tmux new-window`가 같은 프로젝트에 새 탭 생성
- `tmux send-keys` 입력 전달 확인
- `tmux list-panes`가 caller 탭 pane만 노출
- `tmux kill-pane`로 pane + session 정리
- `tmux display-message -p` 포맷 치환 확인
- `tmux has-session` live/dead 컨텍스트 구분 확인
- `Terminal` 프로필에서는 shim env 미주입 확인
