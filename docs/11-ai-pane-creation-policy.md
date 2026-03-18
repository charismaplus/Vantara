# AI Pane Creation Policy

## 1. 목적

이 문서는 AI 또는 사용자 액션으로 생성되는 pane의 생성, 분할, 종료, 번호 부여, 포커스, 폴백 규칙을 정의한다.

목표는 다음과 같다.

- pane 생성 규칙을 예측 가능하게 만든다.
- AI가 `tmux split-window`를 사용할 때 제품이 일관되게 대응하도록 만든다.
- pane identity를 사람이 이해할 수 있는 형태로 안정적으로 노출한다.
- 사용자 조정 레이아웃과 자동 분할 정책이 충돌하지 않게 한다.

이 정책은 mock UI가 아니라 실제 제품 런타임을 기준으로 한다.

## 2. 범위

이 문서는 다음 대상에 적용된다.

- 실제 Tauri desktop runtime
- PTY session lifecycle
- layout tree persistence
- 브라우저 preview mock
- 향후 tmux shim adapter

이 문서는 다음을 직접 구현하지는 않지만 기준을 제공한다.

- Claude Code or Codex 전용 orchestration
- multi-agent planner
- auto-approval flow

## 3. 기본 개념

### 3.1 Pane

pane은 화면상 하나의 leaf terminal slot이다.

규칙:

- `pane = terminal leaf 1개`
- pane은 고정된 내부 `paneId`를 가진다.
- pane은 사람이 읽을 수 있는 고정 번호 `paneOrdinal`을 가진다.
- pane은 사람이 보는 라벨 `paneLabel`을 가진다.

### 3.2 Pane Identity

pane identity는 화면 위치가 아니라 생성 시점에 결정된다.

예:

- 첫 pane: `P1`
- 첫 split로 생성된 새 pane: `P2`
- 그 다음 새 pane: `P3`

중요 규칙:

- pane 번호는 생성 후 절대 바뀌지 않는다.
- 다른 pane이 닫혀도 남은 pane의 번호는 유지된다.
- 같은 tab 안에서만 번호가 유일하면 된다.

### 3.3 Tab Scope

pane 번호는 tab 스코프다.

즉:

- 각 tab은 `nextPaneOrdinal` 카운터를 가진다.
- 새 tab은 항상 `P1`부터 시작한다.
- 다른 tab의 pane 번호와 충돌해도 문제 없다.

## 4. Pane 생성자

pane은 생성 주체를 가진다.

종류:

- `user`
- `ai`

초기 구현에서는 대부분 `user`로 생성되지만, 모델은 `ai` 생성자를 저장할 수 있어야 한다.

필드:

- `createdBy`
- `sourcePaneId`

설명:

- `createdBy`: 누가 pane 생성을 요청했는지
- `sourcePaneId`: 어떤 pane에서 분기되었는지

## 5. AI Pane 요청 입력

AI가 pane 생성을 요청할 때 시스템은 자연어를 해석하는 대신 runtime 신호를 우선 사용한다.

우선순위:

1. tmux CLI 옵션
2. explicit app command
3. policy fallback

예:

- `tmux split-window -h` → 좌우 분할
- `tmux split-window -v` → 위아래 분할
- 옵션 없음 → 자동 방향 결정

## 6. 분할 방향 규칙

### 6.1 명시 방향

- `-h` 또는 이에 대응되는 요청 → 좌우 분할
- `-v` 또는 이에 대응되는 요청 → 위아래 분할

### 6.2 방향이 없는 경우

옵션이 없으면 자동 규칙을 사용한다.

정책:

- 현재 pane가 충분히 넓으면 좌우 분할 우선
- 현재 pane가 좁으면 위아래 분할 우선
- 이미 같은 축으로 과도하게 쪼개졌으면 반대 축 우선
- 최소 크기를 보장할 수 없으면 split 대신 새 tab으로 폴백

초기 구현 fallback:

- 넓이 여유가 크면 좌우
- 그렇지 않으면 위아래

## 7. 기본 분할 비율

새 pane는 보조 작업 성격을 가지는 경우가 많으므로 기존 pane를 조금 더 크게 유지한다.

기본 비율:

- 좌우 분할: `55 / 45`
- 위아래 분할: `60 / 40`

설명:

- 좌우 분할은 가독성을 위해 지나친 폭 손실을 막아야 한다.
- 위아래 분할은 터미널 높이 손실 체감이 더 크므로 기존 pane를 더 크게 유지한다.

규칙:

- 첫 child는 기존 pane
- 둘째 child는 새 pane
- 기존 pane이 항상 더 큰 쪽을 가진다.

## 8. 사용자 조정 우선

사용자가 split 크기를 직접 바꿨다면 그 값이 자동 정책보다 우선한다.

규칙:

- 자동 비율은 pane 생성 직후 1회만 적용한다.
- 이후 사용자가 resize하면 저장된 layout을 사용한다.
- 자식 구조가 바뀌면 그 그룹에 한해 fallback default를 다시 계산한다.

## 9. 최소 크기 정책

자동 split은 사용 가능한 terminal 공간을 깨지 않도록 최소 크기 제한을 가진다.

초기 정책:

- 좌우 분할 최소 비율: `24%`
- 위아래 분할 최소 비율: `20%`

제품 목표:

- 실제 픽셀 기준 최소 폭/높이 규칙도 추후 같이 사용한다.

행동:

- 최소 조건을 만족하지 못하면 split을 거부하지 않고 새 tab 생성으로 폴백할 수 있다.
- 초기 구현에서는 UI panel min size로 우선 방어한다.

## 10. 포커스 정책

pane 생성 시 포커스 정책은 사용자와 AI 요청을 구분할 수 있어야 한다.

권장 정책:

- 사용자 split: 새 pane로 포커스 이동 가능
- AI split: 기존 pane 포커스 유지

초기 구현:

- 현재 포커스 유지
- UI는 새 pane를 생성하지만 강제 focus 이동은 하지 않는다.

## 11. 종료 정책

pane 종료와 session 종료는 분리된 개념이지만, 이 제품의 기본 UX는 pane 중심으로 단순화한다.

기본 정책:

- `Close Pane`는 해당 pane의 terminal session도 함께 종료한다.
- pane이 닫히면 sibling이 공간을 이어받는다.
- 마지막 pane은 닫을 수 없고 빈 pane 상태로 유지한다.

예외:

- 종료된 session을 보고 있는 pane은 재사용 가능하다.

## 12. 번호 정책

pane 번호는 읽기 순서가 아니라 생성 순서 기반이다.

규칙:

- 생성 시 `nextPaneOrdinal`을 할당
- 기존 pane 번호는 불변
- 삭제 후 재사용하지 않음
- 표시 형식은 `P1`, `P2`, `P3`

UI 규칙:

- pane toolbar 앞에 badge로 표시
- session meta에도 같은 번호를 함께 보여줄 수 있다.
- 사용자 명령은 번호 기준으로도 전달 가능해야 한다.

## 13. 영속성 정책

pane state는 앱 실행 중 메모리에만 유지한다. 앱 재실행 후에는 복원하지 않는다.

영속 저장에서 유지할 것:

- tab id
- tab title
- active tab id

영속 저장에서 유지하지 않을 것:

- split tree
- pane sizes
- pane ordinal state
- session attachment state

복원 규칙:

- 앱을 다시 켜면 각 tab은 항상 단일 empty pane으로 시작한다.
- pane 번호는 실행 중 세션에서만 의미를 가진다.
- runtime cache가 살아 있는 동안에는 pane identity와 split metadata를 유지한다.
- 디스크에는 pane layout을 저장하지 않는다.

## 14. tmux Shim 대응 규칙

향후 tmux shim은 이 정책을 그대로 사용해야 한다.

명령 매핑:

- `tmux split-window -h` → `close source? no`, `split horizontal layout`, `createdBy=ai`
- `tmux split-window -v` → `split vertical layout`, `createdBy=ai`
- `tmux send-keys` → target pane session input
- `tmux kill-pane` → target pane close

옵션 없는 split:

- policy fallback 적용

중요:

- AI는 방향을 자연어로 말하지 않아도 된다.
- CLI 옵션이 1차 입력이다.
- 옵션이 없을 때만 제품 정책이 개입한다.

## 15. 초기 구현 범위

이번 구현에서 반드시 반영할 것:

- stable pane ordinal
- runtime pane label stability
- tab-level `nextPaneOrdinal`
- split default ratio
- child structure 변경 시 layout fallback reset
- pane close = session close
- UI에 pane label 노출
- pane layout non-persistence across app relaunch

이번 구현에서 모델만 준비할 것:

- `createdBy`
- `sourcePaneId`

이번 구현에서 아직 제외하는 것:

- 실제 tmux shim adapter
- AI request source tracing
- focus policy 분리
- auto tab fallback when pane too small

## 16. 성공 기준

다음이 만족되면 정책 구현 1차는 성공이다.

- 분할해도 기존 pane 번호가 바뀌지 않는다.
- pane을 닫아도 남은 pane 번호가 유지된다.
- 새 pane는 항상 다음 번호를 받는다.
- 브라우저 preview와 실제 runtime이 같은 pane metadata 구조를 사용한다.
- 새 split은 `55/45` 또는 `60/40` 규칙으로 시작한다.
- 사용자가 resize한 비율은 구조가 바뀌기 전까지 유지된다.
