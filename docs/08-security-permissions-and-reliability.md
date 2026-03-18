# Security, Permissions, and Reliability

## 1. 보안 목표

이 제품은 터미널 앱이므로 기본적으로 강한 권한을 가진다. 따라서 "기능이 된다"보다 "권한 경계가 명확하다"가 더 중요하다.

핵심 원칙:

- 내부 제어면을 최소화한다.
- 불필요한 네트워크 노출을 만들지 않는다.
- 사용자 명시 없이 프로젝트 파일을 수정하지 않는다.
- 세션과 프로젝트 경계를 넘는 자동 행동을 금지한다.

## 2. 보안 경계

### 2.1 UI

- 사용자 입력과 표시만 담당
- 직접 파일 시스템 쓰기 금지

### 2.2 Core

- 프로젝트 메타데이터 저장 허용
- PTY 생성 허용
- settings 저장 허용

### 2.3 Session Process

- 사용자가 선택한 cwd와 shell에서 실행
- 임의 프로젝트 수정은 결국 shell process의 권한 범위에 달려 있음

따라서 앱은 최소한 "앱 자신이 사용자를 대신해 무엇을 자동으로 수정하는지"를 엄격히 제한해야 한다.

## 3. 금지할 동작

초기 제품에서 다음은 금지한다.

- `~/.claude/settings.json` 같은 외부 도구 설정 자동 수정
- 프로젝트 디렉토리에 `.mcp.json`, `CLAUDE.md` 등 파일 자동 생성
- localhost 제어 API 기본 오픈
- 자동 승인 입력 전송
- 사용자의 명시 없는 shell command 실행

이 항목들은 기존 DevHub에서 유용했을 수 있지만, 범용 터미널 제품의 기본 동작이 되어서는 안 된다.

## 4. 권한 모델

### 4.1 프로젝트 등록

사용자가 명시적으로 추가한 디렉토리만 프로젝트로 취급한다.

### 4.2 파일 접근

앱은 메타데이터와 UI 목적의 파일 읽기만 수행한다.

허용:

- 프로젝트 디렉토리 존재 여부 확인
- 아이콘 탐색
- 최근 파일 표시를 위한 제한된 메타데이터 읽기

비허용:

- 프로젝트 내 파일 자동 수정
- 설정 자동 주입

### 4.3 세션 실행

세션은 사용자가 요청한 경우에만 생성한다.

필수 표시 항목:

- shell
- cwd
- title

## 5. IPC 보안

- renderer에서 호출 가능한 command를 whitelist로 제한
- 임의 shell execution command를 generic IPC로 노출하지 않는다
- 모든 command payload를 validation한다
- path 인자는 canonicalize 후 검사한다

## 6. 네트워크 정책

기본 제품은 네트워크 서버를 열지 않는다.

즉:

- 내부 localhost HTTP API 없음
- WebSocket 서버 없음
- SSE 없음

예외가 필요하다면:

- 별도 opt-in feature
- 명시적 포트
- localhost bound
- authentication token

## 7. 프로젝트 경계

프로젝트별 session과 workspace는 논리적으로 분리되어야 한다.

규칙:

- 한 프로젝트의 workspace action이 다른 프로젝트의 layout을 바꾸면 안 된다.
- project switching은 view context change이지 session transfer가 아니다.
- session creation 시 반드시 `projectId`와 유효 cwd가 함께 들어와야 한다.

## 8. 안정성 목표

- UI 재마운트가 session 종료를 유발하면 안 된다.
- pane resize 폭주가 PTY 문제를 일으키면 안 된다.
- 대량 출력이 메모리 폭증으로 이어지면 안 된다.
- 저장 실패 시 workspace 전체가 깨지면 안 된다.

## 9. 종료 안정성

앱 종료 시 우선순위:

1. workspace snapshot flush
2. settings flush
3. session graceful terminate 시도
4. 타임아웃 후 force kill

설정으로 다음 정책을 제공할 수 있다.

- 종료 시 살아 있는 session 유지 여부 확인
- 종료 확인 dialog
- 강제 종료 타임아웃

## 10. 출력 폭주 대응

대량 로그 출력은 UI 병목의 대표 원인이다.

대응:

- session runtime ring buffer 상한
- renderer write batching
- 비가시 pane throttling
- 긴 로그에 대한 lazy replay

## 11. 장애 복구

### 11.1 Renderer 실패

- view 재생성
- session 재부착
- 사용자에게 "view recovered"만 표시

### 11.2 PTY 실패

- session status를 `failed`로 표시
- restart action 제공

### 11.3 Snapshot 손상

- 안전한 기본 탭으로 복귀
- diagnostics 저장

## 12. 로그와 진단

권장 로그:

- command execution log
- session lifecycle log
- persistence error log
- renderer error log

민감 정보 처리:

- 입력 내용 전체를 기본 로그에 남기지 않는다
- env 값 중 비밀값은 마스킹한다
- path는 필요 시 사용자 옵션에 따라 익명화 가능
