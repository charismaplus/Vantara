# Terminal Runtime and Renderer

## 1. 목표

터미널 서브시스템은 제품의 심장이다. 이 문서의 목표는 다음을 명확히 하는 것이다.

- PTY를 누가 만들고 관리하는가
- terminal 화면은 누가 그리는가
- UI 레이아웃과 terminal surface는 어떻게 연결되는가
- Ghostty 계열 네이티브 렌더러를 어떤 추상화 위에 올릴 것인가

## 2. 책임 분리

### 2.1 PTY Runtime

책임:

- shell 프로세스 실행
- stdin/out 연결
- resize
- exit 감지
- env와 cwd 적용

### 2.2 Terminal Renderer

책임:

- 문자를 시각적으로 렌더링
- 스크롤, selection, hyperlink, IME 처리
- focus와 입력 이벤트 연결

### 2.3 Layout Host

책임:

- 화면에서 terminal이 위치할 bounds 계산
- pane visibility와 resize 이벤트 전달

이 세 요소를 분리해야 renderer 교체가 가능하다.

## 3. PTY 런타임 설계

Windows 우선 기준:

- 기본 구현은 ConPTY 사용
- shell profile별 실행 지원
- cwd와 env override 지원
- graceful shutdown과 force kill 분리

세션 생성 입력:

- `projectId`
- `shell`
- `cwd`
- `envOverrides`
- `titleHint`
- `profileId`

세션 출력 처리:

- raw byte stream 수신
- renderer에 chunk 전달
- scrollback 저장
- UI event 발행

## 4. 세션 매니저 요구사항

SessionManager는 최소 다음 API를 가져야 한다.

- `create_session`
- `write_input`
- `resize_session`
- `terminate_session`
- `restart_session`
- `attach_view`
- `detach_view`
- `get_session_snapshot`

추가 규칙:

- session은 view 없이도 생존 가능해야 한다.
- session 출력은 view가 붙기 전에도 버퍼링되어야 한다.
- renderer가 재생성되어도 session은 유지되어야 한다.

## 5. Scrollback 전략

scrollback은 세 층으로 관리한다.

- renderer in-memory buffer
- session runtime ring buffer
- optional persisted scrollback file

권장 정책:

- 메모리 ring buffer를 기본 사용
- 큰 출력은 파일 스냅샷으로 spill 가능
- DB에는 전문 저장하지 않음

이유:

- DB 쓰기 부하 감소
- 렌더러 교체와 독립
- session 재부착 시 마지막 일부 출력 즉시 복원 가능

## 6. Renderer Adapter

UI와 renderer 간 계약은 adapter로 통일한다.

```ts
interface TerminalRendererAdapter {
  mount(host: TerminalHost): Promise<void>;
  unmount(viewId: string): Promise<void>;
  bindSession(viewId: string, sessionId: string): Promise<void>;
  unbindSession(viewId: string): Promise<void>;
  write(viewId: string, chunk: Uint8Array | string): Promise<void>;
  resize(viewId: string, cols: number, rows: number, rect: Rect): Promise<void>;
  focus(viewId: string): Promise<void>;
  setTheme(viewId: string, theme: TerminalTheme): Promise<void>;
}
```

핵심 포인트:

- UI는 renderer 종류를 몰라야 한다.
- Core는 renderer의 구현 세부를 몰라야 한다.
- session output은 adapter를 통해 view로 간다.

## 7. Ghostty 통합 전략

### 7.1 현실 전제

Ghostty 계열 렌더러는 DOM 안에 직접 넣는 웹 컴포넌트가 아니다. 따라서 `xterm.js`처럼 단순히 `<div>` 하나에 붙이는 방식과 동일하게 취급하면 안 된다.

### 7.2 목표 구조

UI는 pane의 위치와 크기를 계산한다.
네이티브 layer는 해당 위치에 terminal native surface를 올린다.

즉 UI는 "terminal hole punching layout"을 제공하고, renderer는 그 홀에 surface를 맞춰 붙는 구조가 된다.

### 7.3 구현 전략

- UI는 각 terminal host의 화면 좌표와 크기를 IPC로 전달한다.
- Desktop shell/native layer는 해당 rect에 native terminal view를 배치한다.
- window resize, pane split, drag 이동 시 rect를 재계산한다.
- view visibility가 false인 pane은 renderer surface도 비활성화한다.

## 8. xterm.js의 위치

제품 목표는 Ghostty 계열 네이티브 렌더러를 우선하는 것이지만, 아키텍처 검증과 개발용 fallback을 위해 `xterm.js` adapter를 유지하는 것이 합리적이다.

중요한 점:

- `xterm.js`가 제품 구조를 결정해서는 안 된다.
- pane tree와 IPC는 native renderer 기준으로 설계한다.
- fallback renderer는 adapter 계약을 만족시키는 구현체일 뿐이다.

## 9. 입력 처리

입력은 다음 경로를 따른다.

1. 사용자가 terminal view에 포커스
2. renderer가 키 입력/IME 입력을 수집
3. adapter가 core의 `write_input(sessionId, bytes)` 호출
4. PTY에 전달

복사/붙여넣기:

- selection이 있으면 copy
- selection 없으면 `Ctrl+C`는 신호 여부를 설정으로 제어
- paste는 chunk 단위로 전송

## 10. Resize 처리

resize는 단순 CSS 문제가 아니라 terminal correctness 문제다.

순서:

1. pane rect 계산
2. renderer 측에서 사용 가능한 pixel size 계산
3. 글꼴 metrics 기준으로 cols/rows 계산
4. core에 resize 전달
5. PTY resize

여기서 핵심은 cols/rows가 renderer와 PTY 사이에서 항상 일치해야 한다는 점이다.

## 11. 제목 정책

session title은 여러 출처가 있다.

- 사용자가 지정한 title
- shell profile 이름
- PTY가 보고한 window title
- cwd 기반 자동 title

우선순위:

1. 사용자 지정 title
2. 명시적 titleHint
3. 자동 생성 title

## 12. 종료 정책

종료는 두 단계가 필요하다.

- graceful terminate
- force kill

graceful terminate 예:

- `Ctrl+C`
- shell exit command
- 일정 시간 대기

그 후 실패 시 force kill

## 13. 세션 복원 정책

복원에는 두 종류가 있다.

- UI 복원
- process 복원

UI 복원:

- 마지막 탭, pane tree, stack 상태 복원

process 복원:

- 앱 재실행 후 이전 session을 동일 프로세스로 되살리는 것은 일반적으로 불가능하다

따라서 제품 기본 정책은 다음이 되어야 한다.

- 앱이 살아 있는 동안은 session 유지
- 앱 재실행 후에는 session layout만 복원하고, 필요한 세션은 재시작 상태로 보여준다

이 구분을 UX 문구에 명확히 반영해야 한다.

## 14. 성능 요구사항

- 빠른 타이핑 시 입력 지연이 체감되지 않아야 한다.
- 대량 출력에서도 메인 UI 렌더가 막히면 안 된다.
- 비활성 프로젝트의 renderer 자원은 축소 가능해야 한다.
- view가 보이지 않을 때는 불필요한 repaint를 줄여야 한다.
