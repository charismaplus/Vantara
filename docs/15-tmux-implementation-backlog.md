# tmux Implementation Backlog

## 목적

이 문서는 Workspace Terminal에서 `tmux` 호환 shim을 어디까지 구현했는지, 그리고 앱 모델 안에서 추가 구현이 가능한 범위를 어떤 순서로 닫아갈지 기록하는 작업 문서다.

기준 원칙:

- 가능한 경우는 tmux semantics를 그대로 따른다.
- 앱 모델과 충돌하는 영역은 degraded 또는 unsupported로 남긴다.
- "구현 가능"은 현재 제품 구조인 `Project -> Session -> Window -> Pane` 안에서 의미 있게 매핑 가능한 경우를 뜻한다.

## 완료된 태스크

### T1. Session / Window / Pane Core

상태: 완료

- `new-session`
- `attach-session`
- `switch-client`
- `kill-session`
- `list-sessions`
- `rename-session`
- `has-session`
- `new-window`
- `list-windows`
- `select-window`
- `kill-window`
- `rename-window`
- `move-window`
- `swap-window`
- `rotate-window`
- `split-window`
- `kill-pane`
- `select-pane`
- `resize-pane`
- `capture-pane`
- `send-keys`
- `display-message`

메모:

- `session = WorkspaceSession`
- `window = 상단 탭`
- `pane = split leaf`
- exact target/direction/size 정보가 있으면 heuristic보다 우선한다.

### T2. Pane / Window Rearrangement

상태: 완료

- `break-pane`
- `join-pane`
- `move-pane`
- `swap-pane`
- `respawn-pane`
- `respawn-window`
- `pipe-pane`

메모:

- 현재 앱 레이아웃 모델에 맞춰 동작한다.
- tmux의 일부 edge case는 degraded일 수 있다.

### T3. Synthetic Option / Environment Surface

상태: 완료

- `set-option`
- `show-options`
- `set-window-option`
- `show-window-options`
- `set-environment`
- `show-environment`

메모:

- runtime synthetic state에 유지된다.
- 실제 tmux server option storage와는 다르다.

### T4. Synthetic Buffer / Hook / Binding Surface

상태: 완료

- `bind-key`
- `unbind-key`
- `list-keys`
- `set-hook`
- `show-hooks`
- `set-buffer`
- `show-buffer`
- `list-buffers`
- `delete-buffer`
- `load-buffer`
- `save-buffer`
- `paste-buffer`
- `wait-for`
- `source-file`

메모:

- buffer는 text buffer 기준으로 synthetic state에 저장된다.
- `paste-buffer`는 target pane input으로 주입된다.
- `wait-for`는 in-memory synchronization primitive로 구현된다.
- `source-file`은 지원 명령 subset을 line-by-line로 재귀 실행한다.

## 다음 구현 후보

### T5. Listing / Diagnostics Expansion

상태: 후보

- `show-messages`
- `show-hooks` format 확장
- `list-buffers` format 확장
- `display-message` format 변수 추가

메모:

- 현재도 구현은 가능하지만, 실제 AI CLI 호환성 대비 우선순위는 낮다.

### T6. Clipboard / Buffer Interop

상태: 후보

- tmux buffer와 앱 clipboard bridge
- `save-buffer -` / stdout surface
- binary buffer fallback 정책

메모:

- 현재 제품은 OS clipboard와 terminal paste path를 이미 갖고 있다.
- tmux buffer까지 완전 통합하려면 추가 정책 결정이 필요하다.

## 현재 구조상 보류 또는 unsupported

### U1. True tmux Server / Socket Model

상태: unsupported

- `kill-server`
- real tmux socket path
- multi-client attach graph
- control mode fidelity

이유:

- Workspace Terminal은 tmux server가 아니라 별도 UI shell이다.

### U2. Full Interactive tmux UI Modes

상태: unsupported

- `copy-mode`
- `choose-tree`
- `choose-buffer`
- `choose-client`
- `display-menu`
- `display-popup`
- `command-prompt`

이유:

- tmux 내장 TUI를 현재 앱의 GUI surface 위에 exact하게 재현하지 않는다.

### U3. Shared Window Graph Semantics

상태: unsupported

- `link-window`
- `unlink-window`

이유:

- 현재 window는 특정 session의 소유 객체다.
- tmux처럼 여러 session이 같은 window object를 공유하는 모델이 없다.

## 구현 순서 원칙

1. CLI agent orchestration에 직접 영향을 주는 명령을 먼저 구현한다.
2. exact semantics를 표현할 수 있는 정보가 있으면 degraded fallback을 쓰지 않는다.
3. synthetic surface는 명시적으로 synthetic임을 유지하되, 성공처럼 보이는 no-op는 줄인다.
4. 현재 앱 모델과 충돌하는 명령은 억지 매핑보다 explicit unsupported가 낫다.
