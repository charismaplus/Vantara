# tmux Command Flags and Compatibility Policy

## 1. 목적

이 문서는 tmux 전체 명령 집합을 Workspace Terminal 관점에서 해석하기 위한 기준 문서다.

목표는 세 가지다.

- tmux 명령과 플래그의 실제 의미를 공식 문서 기준으로 정리한다.
- 현재 Workspace Terminal shim이 어디까지 반영하고 어디서부터 의미를 잃는지 매핑한다.
- 완전 호환이 가능한 경우는 그대로 따르고, 불가능한 경우에만 합리적 fallback을 적용하는 정책을 정의한다.

특히 현재 논쟁인 `tmux split-window -h` 반복 호출의 의미를 대표 사례로 다룬다.

## 2. 조사 기준과 소스

문서 기준 소스:

- 현재 기준 man page: [OpenBSD current tmux.1](https://man.openbsd.org/tmux.1)
- 레거시 비교 기준: [OpenBSD 6.8 tmux.1](https://man.openbsd.org/OpenBSD-6.8/tmux.1)
- 레거시 동작 참고: [OpenBSD 5.9 tmux.1](https://man.openbsd.org/OpenBSD-5.9/man1/tmux.1)
- 현재 코드 기준:
  - [apps/desktop/src-tauri/src/bin/tmux.rs](../apps/desktop/src-tauri/src/bin/tmux.rs)
  - [apps/desktop/src-tauri/src/main.rs](../apps/desktop/src-tauri/src/main.rs)
  - [docs/test/conversation_log.md](./test/conversation_log.md)

주의:

- tmux는 버전 간 플래그와 format 변수, default behavior가 미세하게 달라질 수 있다.
- 특히 `split-window`, `new-window`, `send-keys`, format expansion, target 해석은 레거시 버전과 current 사이에 차이가 있다.
- shim은 tmux 전체를 흉내 내는 것이 아니라, tmux surface를 사용하는 CLI agent가 문제없이 동작하도록 필요한 subset을 구현하는 것이 1차 목표다.

## 3. tmux 모델 기초

### 3.1 핵심 객체

- `server`: tmux 전체 프로세스와 socket
- `session`: window 집합과 작업 컨텍스트
- `window`: pane 집합과 layout 단위
- `pane`: 실제 terminal leaf
- `client`: 어떤 terminal이 어떤 session/window를 보고 있는지 나타내는 뷰

### 3.2 active / current / target

- `active pane`: 해당 window 안에서 현재 선택된 pane
- `current window`: session 안에서 현재 보고 있는 window
- `current session`: client가 붙어 있는 session
- `target`: 명령이 조작할 session/window/pane

tmux는 명령마다 기본 target을 다르게 잡는다.

- 일부 명령은 current pane을 기본으로 사용한다.
- 일부 명령은 current window 또는 current session을 기본으로 사용한다.
- `-t`, `-s`, positional target이 있으면 그 값이 우선한다.

Workspace Terminal shim은 tmux의 full client model을 모두 재현하지 않으므로, 기본 target은 다음 순서로 해석해야 한다.

1. 명시적 `-t`
2. caller session이 속한 pane
3. caller tab/window 문맥

즉, 현재 제품에서는 `caller pane context`가 authoritative source다.

### 3.3 pane split과 focus

`split-window`는 단순히 "새 pane를 만든다"가 아니라 아래를 함께 바꿀 수 있다.

- 어떤 pane을 기준으로 split할지
- 어느 축으로 split할지
- 새 pane를 target 앞에 둘지 뒤에 둘지
- leaf split인지 full-span split인지
- split 후 focus가 이동할지 유지될지
- split 결과를 어떤 format으로 출력할지

따라서 단순히 "pane 하나 더 추가"로 해석하면 tmux 호환성이 깨진다.

## 4. 전역 문법과 target/format/size 규칙

### 4.1 target 문법

tmux는 session/window/pane target을 문자열로 넘긴다.

대표적인 토큰:

- pane id: `%<pane-id>`
- window id: `@<window-id>`
- session id: `$<session-id>`
- index 기반 참조: `session:window.pane`, `window.pane`, `pane-index`
- 상대 참조: 현재, 이전, 다음, 마지막 같은 문맥 토큰

shim v1에서는 아래만 exact 또는 normalized 대상으로 삼는 것이 현실적이다.

- `%pane-id`
- caller tab 기준 pane index
- caller tab 내부의 직접 pane id

다른 session/window를 가리키는 full tmux target grammar는 v1에서 explicit fail 또는 degraded로 두는 편이 안전하다.

### 4.2 format 문자열

tmux는 `#{...}` 형식의 format expansion을 광범위하게 사용한다.

핵심 포인트:

- `display-message -p`
- `list-panes -F`
- `new-window -P -F`
- `split-window -P -F`

현재 shim이 우선 지원해야 하는 최소 format 변수:

- `#{pane_id}`
- `#{pane_index}`
- `#{pane_title}`
- `#{pane_current_command}`
- `#{window_index}`
- `#{window_id}`
- `#{session_name}`

이외 변수는 존재하더라도 v1에서는 대부분 unsupported 또는 empty expansion으로 처리해야 한다.

### 4.3 size와 placement

tmux pane split 결과는 direction만으로 결정되지 않는다.

중요 개념:

- `-h`, `-v`: split axis
- `-l size`: absolute size 또는 current 버전 기준 `%`가 붙은 size
- legacy `-p percentage`: 레거시 percentage split
- `-f`: leaf split이 아니라 full width/full height split
- `-b`: 새 pane을 target 앞(before)에 배치
- `-d`: split 후 focus 이동 억제

정리:

- size 정보가 있으면 layout heuristic보다 size 의미가 우선이다.
- `-f`와 `-b`는 단순 decoration이 아니라 layout topology를 바꾸는 플래그다.
- size 정보가 없는 경우에만 equal split 또는 default split fallback을 허용할 수 있다.

## 5. tmux 전체 command family 카탈로그

이 섹션은 tmux 전체를 family 단위로 정리한다. 목적은 "모든 명령을 다 구현한다"가 아니라, 어떤 family를 exact / degraded / unsupported로 둘지 체계적으로 정하는 데 있다.

### 5.1 Session / Window / Pane Lifecycle

대표 명령:

- `new-session`
- `kill-session`
- `kill-server`
- `new-window`
- `kill-window`
- `split-window`
- `kill-pane`
- `respawn-pane`
- `respawn-window`
- `break-pane`
- `join-pane`
- `swap-pane`
- `swap-window`
- `move-window`
- `unlink-window`
- `link-window`

shim 구현 가치:

- `split-window`, `new-window`, `kill-pane`, `kill-window`는 Core
- `break-pane`, `join-pane`, `respawn-pane`, `swap-pane`는 Second-phase
- full session management는 v1 shim에서 Out of scope

### 5.2 Layout / Focus / Navigation

대표 명령:

- `select-pane`
- `select-window`
- `last-pane`
- `last-window`
- `next-window`
- `previous-window`
- `select-layout`
- `next-layout`
- `resize-pane`
- `rotate-window`
- `swap-pane`

중요 플래그:

- `select-pane`: 방향 선택, last pane, input enable/disable, zoom
- `resize-pane`: 방향, absolute width/height, resize by delta, zoom

shim 구현 가치:

- `select-pane`, `resize-pane`는 Core지만 exact보다는 degraded 또는 partial exact가 현실적
- layout rotation, choose layout family는 Second-phase

### 5.3 Input / Output / Capture

대표 명령:

- `send-keys`
- `capture-pane`
- `pipe-pane`
- `clear-history`
- `display-message`
- `refresh-client`

중요 플래그:

- `send-keys`: literal, hex, repeat, mouse, copy-mode command injection
- `capture-pane`: alternate screen, joined lines, escape handling, start/end range
- `pipe-pane`: pane output/input piping

shim 구현 가치:

- `send-keys`, `display-message`, `capture-pane`는 Core
- `pipe-pane`는 Second-phase
- `refresh-client`는 client model 부재 때문에 degraded 또는 unsupported

### 5.4 Listing / Introspection / Formatting

대표 명령:

- `list-panes`
- `list-windows`
- `list-sessions`
- `list-clients`
- `show-options`
- `show-window-options`
- `show-environment`
- `show-hooks`
- `show-messages`

shim 구현 가치:

- `list-panes`, `list-windows`, `display-message`는 Core
- 나머지는 Second-phase 또는 explicit fail

### 5.5 Options / Environment / Config / Hooks

대표 명령:

- `set-option`, `show-options`
- `set-window-option`, `show-window-options`
- `set-environment`, `show-environment`
- `source-file`
- `bind-key`, `unbind-key`
- `set-hook`, `show-hooks`
- `wait-for`

shim 구현 가치:

- 대부분은 Core가 아니다
- no-op로 두더라도 실제 CLI 동작에 영향이 작은 경우만 safe no-op
- `source-file`와 option 계열은 "성공처럼 보이는 no-op"가 오히려 위험할 수 있다

### 5.6 Buffers / Clipboard / Paste

대표 명령:

- `load-buffer`
- `save-buffer`
- `set-buffer`
- `show-buffer`
- `delete-buffer`
- `choose-buffer`
- `paste-buffer`

shim 구현 가치:

- CLI agent orchestration 기준으로는 대부분 Second-phase 이하
- 현재 제품의 clipboard 모델과 충돌할 수 있으므로 direct mapping은 신중해야 한다

### 5.7 UI / Modes / Menus / Prompts

대표 명령:

- `copy-mode`
- `choose-tree`
- `choose-client`
- `choose-buffer`
- `command-prompt`
- `confirm-before`
- `display-menu`
- `display-popup`
- `clock-mode`

shim 구현 가치:

- 대부분 Out of scope for shim v1
- terminal-integrated UI surface를 완전 재현하지 못하면 explicit fail이 safer

### 5.8 Clients / Attach / Server / Control Mode

대표 명령:

- `attach-session`
- `detach-client`
- `switch-client`
- `lock-client`
- `lock-session`
- `refresh-client`
- control mode 관련 플래그

shim 구현 가치:

- Workspace Terminal은 tmux 자체가 아니라 별도 UI shell이므로 direct emulation 대상이 아니다
- 대체로 Out of scope

## 6. 현재 Workspace Terminal shim 매핑

### 6.1 현재 지원 명령

| 명령 | 현재 shim 파싱 | 현재 백엔드 동작 | 등급 | 메모 |
| --- | --- | --- | --- | --- |
| `split-window` | `-h -v -c -n -t -P` 파싱, `-F -l -p -e -d`는 무시 | 현재는 caller tab 내부 AI workspace 경로로 재해석 | `Degraded` | explicit size/placement를 보존하지 못함 |
| `new-window` | `-c -n` 파싱, `-t -F -d`는 무시 | 새 탭 생성 후 AI root pane에서 child session 시작 | `Degraded` | tmux current window/client semantics 미반영 |
| `send-keys` | `-t -l` 파싱 | limited key map을 plain input으로 전송 | `Normalized` for plain input, otherwise `Unsupported` | `-H -X -N -M -R -F` 없음 |
| `list-panes` | `-F -a -t` 파싱 | 실제 scope는 caller tab만 | `Degraded` | `-a`와 외부 target을 제대로 반영하지 않음 |
| `kill-pane` | `-t` 파싱 | pane close + session terminate | `Normalized` | caller tab 범위로 제한 |
| `kill-window` | alias로 받음 | 내부적으로 `kill-pane`와 동일 경로 | `Degraded` | window semantics와 다름 |
| `display-message` | `-p -F -t` 파싱 | limited format variables만 지원 | `Degraded` | non-caller target 반영 약함 |
| `has-session` | 인자 없음 | caller session alive 여부만 확인 | `Degraded` | arbitrary session check 아님 |

### 6.2 현재 no-op 처리

현재 shim이 성공으로 넘기지만 의미를 거의 반영하지 않는 명령:

- `select-pane`
- `resize-pane`
- `set-option`
- `set-window-option`
- `bind-key`
- `unbind-key`
- `set-environment`
- `source-file`

정책 판단:

- `bind-key`, `unbind-key`는 safe no-op에 가깝다.
- `set-option`, `set-window-option`, `source-file`, `set-environment`는 성공처럼 보이는 no-op가 실제 동작 오판을 만들 수 있다.
- `select-pane`, `resize-pane`는 orchestration에서는 꽤 중요하므로 no-op보다는 partial implement가 낫다.

### 6.3 현재 미지원 family

명시적 실패 또는 사실상 미지원 상태:

- `capture-pane`
- `list-windows`
- `select-window`
- `break-pane`
- `join-pane`
- `respawn-pane`
- `swap-pane`
- control mode
- copy-mode / choose-tree / popup 계열

### 6.4 현재 코드 기준 추가 관찰

- shim env는 `TMUX`와 custom env를 세팅하지만 `TMUX_PANE` 같은 주변 호환 env는 제공하지 않는다.
- target 해석은 caller tab 내부 pane id 또는 index까지만 지원한다.
- pane index 해석은 0-based와 1-based를 모두 느슨하게 수용하려고 하지만, tmux의 exact numbering contract와는 다를 수 있다.
- 현재 tmux split 경로는 strict tmux leaf split이 아니라 AI workspace policy로 재해석하는 코드가 포함돼 있다. 이 경로는 layout readability를 높일 수는 있지만 tmux exact semantics는 아니다.

## 7. `split-window -h` 반복 호출 사례 분석

### 7.1 tmux에서 실제로 중요한 것

`split-window -h` 반복 호출의 결과를 결정하는 핵심은 아래 네 가지다.

- target pane이 무엇인가
- split 후 active pane이 어디로 이동하는가
- `-d`가 있는가
- `-f` 또는 size/placement 정보가 있는가

즉, `-h`만 보고 "항상 평평한 2열, 3열, 4열"이 된다고 기대하면 안 된다.

### 7.2 가능한 tmux 결과

시나리오 A: 같은 target pane을 계속 `-t`로 지정

- 같은 pane이 반복적으로 좌우 split된다.
- 결과는 한 leaf가 계속 잘리는 형태가 된다.

시나리오 B: `-t` 없이 split 후 새 pane가 current pane이 되는 흐름

- 매번 "방금 생성된 pane"이 다시 쪼개진다.
- 사용자가 보기엔 프랙탈처럼 보이는 leaf recursion이 나온다.

시나리오 C: `-f -h`

- target leaf가 아니라 window 기준 full-height split semantics가 적용된다.
- 반복 호출 시 컬럼 증설에 가까운 결과가 나온다.

### 7.3 현재 Workspace Terminal 문제

현재 제품은 tmux exact semantics를 그대로 따르기보다, AI workspace zone으로 split 요청을 재배치하는 경로가 있다.

이 접근의 장점:

- AI pane이 너무 작은 leaf를 연쇄적으로 만드는 현상을 줄일 수 있다.

단점:

- explicit target, size, placement 정보가 있어도 tmux와 다른 topology가 만들어질 수 있다.
- 사용자는 "tmux를 지원한다"고 느끼지만 실제로는 앱 정책이 tmux 의미를 덮어쓰게 된다.

결론:

- `split-window -h` 반복이 프랙탈처럼 보이는 것 자체는 tmux에서 가능하다.
- 하지만 현재 제품이 exact tmux path와 workspace-friendly path를 구분하지 않는다면, 사용자는 어떤 경우가 tmux 본래 의미인지 알 수 없게 된다.

## 8. 권장 호환 정책

### 8.1 최상위 원칙

1. 정확한 tmux 의미를 표현할 수 있으면 그대로 따른다.
2. 정보가 부족하거나 앱 모델이 직접 표현하지 못할 때만 fallback을 쓴다.
3. explicit target, direction, size, placement를 UI readability 때문에 덮어써서는 안 된다.

등급 정의:

- `Exact`
- `Normalized`
- `Degraded`
- `Unsupported`

### 8.2 `split-window` 정책

#### Exact path

아래 정보가 있으면 strict tmux path를 사용한다.

- target pane
- direction
- size or default equal split rule
- `-f` / `-b` / `-d` 여부

처리 규칙:

- `-t`가 있으면 그 pane이 authoritative target
- `-t`가 없으면 caller pane이 target
- `-h/-v`는 그대로 따른다
- `-l size`와 legacy `-p percentage`는 내부 normalized size spec으로 변환한다
- `-f`는 leaf split이 아니라 caller window/tab root 기준 full-span split으로 처리한다
- `-b`는 before placement로 처리한다
- `-d`는 active pane 유지로 처리한다
- `-P/-F`는 반드시 result metadata 출력 계약으로 처리한다
- `-e`는 child session env override로 반영한다

#### Degraded path

아래 조건에서만 degraded를 허용한다.

- target이 명시되지 않았고 caller pane 외 다른 current-pane semantics를 복원할 수 없음
- size semantics를 현 모델로 정확히 표현할 수 없음
- full client model이 없어 current client/window를 확정할 수 없음

degraded 기본값:

- target pane은 caller pane 유지
- requested axis는 유지
- size가 없으면 equal split
- placement가 불명확하면 after placement
- degraded가 발동한 이유를 로그에 남김

중요:

- `AI workspace rebalance`는 exact tmux path에서는 사용하면 안 된다.
- 이런 정책은 별도 app split mode가 필요할 때만 사용해야 한다.

### 8.3 `new-window` 정책

- `new-window`는 same project 안의 새 탭으로 매핑 가능하다.
- 그러나 tmux의 current session/window targeting을 모두 재현하지 못하므로 `Normalized`로 보는 것이 안전하다.
- `-d`, `-n`, `-c`, `-P`, `-F`, `-t`는 최소한 parse 후 의미를 문서화해야 한다.
- `-P/-F`를 받았으면 생성된 tab/pane/session metadata를 format expansion으로 반환하는 것이 맞다.

### 8.4 `send-keys` 정책

- plain text와 일반 제어키는 `Exact` 또는 `Normalized`로 구현 가능하다.
- copy-mode command injection(`-X`), hex input, repeat count, mouse forwarding은 별도 path가 필요하다.
- v1에서는 아래로 나누는 것이 현실적이다.
  - plain text / Enter / Tab / Ctrl keys: implement
  - `-X` copy-mode command: explicit fail
  - `-H`, `-N`, `-M`, `-F`, `-R`: explicit fail 또는 degraded 금지

### 8.5 `list-panes` / `display-message`

- format variable subset은 exact하게 지원하고, 나머지는 explicit unsupported variable 또는 empty expansion으로 일관 처리해야 한다.
- caller tab 범위만 허용하는 것은 괜찮지만, 그 제한은 문서와 에러 메시지에 명시해야 한다.

### 8.6 `select-pane` / `resize-pane`

이 둘은 더 이상 safe no-op로 두면 안 된다.

- `select-pane`: 최소한 target pane 선택과 active pane 이동은 구현해야 한다.
- `resize-pane`: absolute/relative resize를 현재 layout tree에 매핑하는 partial exact path가 필요하다.

### 8.7 No-op와 explicit fail의 기준

safe no-op 후보:

- `bind-key`
- `unbind-key`

explicit fail 권장:

- `source-file`
- `set-option`
- `set-window-option`
- `set-environment`
- `copy-mode` UI 계열
- full control-mode 계열

이유:

- 사용자나 CLI가 설정이 적용됐다고 오판하면 이후 동작이 더 불투명해진다.

## 9. 구현 우선순위 제안

### Core for CLI agent orchestration

- `split-window`
- `new-window`
- `send-keys`
- `list-panes`
- `list-windows`
- `display-message`
- `has-session`
- `kill-pane`
- `kill-window`
- `select-pane`
- `resize-pane`
- `capture-pane`

### Useful but second-phase

- `select-window`
- `rename-window`
- `respawn-pane`
- `respawn-window`
- `swap-pane`
- `join-pane`
- `break-pane`
- `move-window`
- `pipe-pane`
- `show-options`
- `show-environment`

### Out of scope for shim v1

- full client/session attach model
- full control mode fidelity
- choose-tree / choose-buffer / popup / menu UI
- copy-mode UI and mode-specific command set
- terminal feature negotiation and advanced hooks

## 10. 최종 결론

- tmux는 "pane를 예쁘게 배치하는 도구"가 아니라 "target pane/window/client semantics를 엄격하게 해석하는 multiplexer"다.
- 따라서 Workspace Terminal shim도 exact path가 가능한 경우에는 tmux를 그대로 따라야 한다.
- `split-window -h` 반복이 프랙탈처럼 보이는 것은 tmux에서 충분히 가능한 결과다.
- 문제는 그 현상 자체가 아니라, 앱이 explicit target/size/placement를 무시하고 독자 정책으로 재해석하는 순간 사용자가 tmux와 다른 결과를 받는다는 점이다.
- 권장 방향은 다음과 같다.
  - tmux path와 app-friendly path를 명확히 분리한다.
  - tmux path에서는 strict target/direction/size semantics를 따른다.
  - 정보가 부족할 때만 degraded fallback을 적용한다.
  - 현재 AI workspace rebalance는 tmux exact path가 아니라 별도 app policy로 분리하는 것이 맞다.
