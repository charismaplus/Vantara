# Data Storage and State

## 1. 저장 전략 목표

이 제품의 데이터는 크게 두 종류다.

- 오래 살아야 하는 구조 데이터
- 런타임에만 의미 있는 동적 데이터

이 둘을 섞으면 복원과 디버깅이 어려워진다.

## 2. 상태 계층

### 2.1 Persistent State

앱 재실행 후에도 유지되어야 하는 데이터:

- 프로젝트 목록
- 프로젝트 메타데이터
- 워크스페이스 탭 구조
- pane layout tree
- 사용자 설정
- 최근 활성 탭/포커스 정보

### 2.2 Runtime State

앱 프로세스가 살아 있는 동안만 유지되는 데이터:

- session process handle
- 현재 PTY 상태
- 출력 버퍼
- live focus
- renderer attachment 정보

### 2.3 Derived UI State

렌더링 편의를 위해 UI가 계산한 상태:

- project filter 결과
- drag target
- command palette results
- context menu anchor

## 3. 저장 기술

권장:

- SQLite: 구조 데이터
- 파일 기반 저장: 대형 scrollback 스냅샷, 로그
- 메모리 캐시: live output buffer

이유:

- SQLite는 관계형 구조와 snapshot 조회에 적합
- scrollback 전문은 DB보다 파일이 운영상 유리

## 4. 제안 스키마

### projects

- `id TEXT PRIMARY KEY`
- `name TEXT NOT NULL`
- `path TEXT NOT NULL UNIQUE`
- `icon TEXT`
- `color TEXT`
- `tags_json TEXT`
- `sort_order INTEGER NOT NULL`
- `last_opened_at TEXT`
- `created_at TEXT NOT NULL`
- `archived_at TEXT`

### workspaces

- `id TEXT PRIMARY KEY`
- `project_id TEXT NOT NULL UNIQUE`
- `active_tab_id TEXT`
- `created_at TEXT NOT NULL`
- `updated_at TEXT NOT NULL`

### tabs

- `id TEXT PRIMARY KEY`
- `workspace_id TEXT NOT NULL`
- `title TEXT NOT NULL`
- `root_layout_json TEXT NOT NULL`
- `active_pane_id TEXT`
- `sort_order INTEGER NOT NULL`
- `created_at TEXT NOT NULL`
- `updated_at TEXT NOT NULL`

### sessions

- `id TEXT PRIMARY KEY`
- `project_id TEXT NOT NULL`
- `title TEXT`
- `shell TEXT NOT NULL`
- `cwd TEXT NOT NULL`
- `env_profile_id TEXT`
- `last_known_status TEXT NOT NULL`
- `last_exit_code INTEGER`
- `last_started_at TEXT`
- `last_ended_at TEXT`
- `scrollback_ref TEXT`

### settings

- `key TEXT PRIMARY KEY`
- `value_json TEXT NOT NULL`
- `updated_at TEXT NOT NULL`

## 5. 왜 layout tree를 JSON으로 저장하는가

pane tree는 관계형 테이블로 완전 정규화할 수도 있지만, V1에서는 탭별 layout snapshot을 JSON으로 저장하는 편이 더 유리하다.

장점:

- split/stack 트리 저장이 단순함
- undo/redo snapshot 처리 쉬움
- schema migration이 비교적 유연함

주의:

- JSON 구조는 contracts 패키지의 타입과 동일하게 버전 관리한다.

## 6. 저장 타이밍

### 즉시 저장

- 프로젝트 생성/삭제
- 설정 변경

### debounce 저장

- 탭 전환
- pane split/resize
- stack 변경
- 포커스 이동

### 종료 시 flush

- 마지막 workspace snapshot
- 미반영 설정

## 7. 세션 복원 데이터

앱 재실행 후에는 live process를 그대로 복원할 수 없기 때문에 "relaunchable session record"를 남겨야 한다.

필요 정보:

- shell
- cwd
- title
- env profile
- 마지막 종료 상태

UI 복원 시:

- 이전에 실행 중이던 세션은 "disconnected/exited" 상태로 나타날 수 있다.
- 사용자가 `Restart`를 누르면 동일 구성으로 다시 시작한다.

## 8. 설정 모델

설정은 섹션 단위 JSON 저장을 권장한다.

예:

- `appearance`
- `terminal`
- `keyboard`
- `projects`
- `advanced`

### appearance

- theme
- accent color
- density

### terminal

- font family
- font size
- line height
- cursor style
- scrollback limit
- copyOnSelect

### keyboard

- keybinding map

### advanced

- autoRestoreWorkspace
- closeBehavior
- confirmBeforeKill

## 9. 마이그레이션 전략

SQLite schema migration과 layout JSON version migration을 분리한다.

원칙:

- DB migration은 숫자 버전
- JSON snapshot은 내부 `schemaVersion` 필드

## 10. 스냅샷 무결성

복원 시 다음 검증이 필요하다.

- root node 존재 여부
- active pane이 tree 안에 존재하는지
- stack active item이 실제 item과 매칭되는지
- sessionId 참조가 유효한지

복원 실패 시:

- 전체 workspace를 버리지 말고 안전한 기본 탭 하나를 생성
- 손상된 snapshot은 별도 diagnostics로 남긴다

## 11. 진단 데이터

개발과 운영을 위해 별도 로그 파일 저장이 필요하다.

권장 항목:

- app log
- session lifecycle log
- renderer attach/detach log
- persistence migration log

이 로그는 product support와 crash 분석에 중요하다.
