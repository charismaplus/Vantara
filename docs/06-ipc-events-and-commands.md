# IPC Events and Commands

## 1. 목표

UI와 core 사이의 계약을 명확한 typed interface로 고정한다.

원칙:

- stringly typed ad-hoc message 금지
- payload schema 명시
- 명령과 이벤트를 분리
- 장기적으로 versioning 가능하게 설계

## 2. 통신 분류

### 2.1 Command

상태 변경 요청. UI 또는 데스크톱 셸이 core에 보낸다.

예:

- 프로젝트 생성
- 탭 생성
- 세션 시작
- pane split

### 2.2 Query

현재 상태 조회. 부작용이 없어야 한다.

예:

- 프로젝트 목록
- workspace snapshot
- session detail

### 2.3 Event

core가 비동기적으로 UI에 푸시하는 상태 변화다.

예:

- session output chunk
- session exit
- workspace updated

## 3. Command 목록

### Project Commands

- `project.create`
- `project.update`
- `project.archive`
- `project.delete`
- `project.reorder`
- `project.open`

### Workspace Commands

- `workspace.open`
- `workspace.restoreLast`
- `workspace.setActiveTab`

### Tab Commands

- `tab.create`
- `tab.rename`
- `tab.close`
- `tab.duplicate`
- `tab.move`

### Layout Commands

- `layout.splitPane`
- `layout.closePane`
- `layout.resizeSplit`
- `layout.moveStackItem`
- `layout.setActiveStackItem`
- `layout.focusPane`
- `layout.attachSession`
- `layout.detachSession`

### Session Commands

- `session.create`
- `session.writeInput`
- `session.resize`
- `session.terminate`
- `session.forceKill`
- `session.restart`
- `session.updateMeta`

### Settings Commands

- `settings.get`
- `settings.update`
- `settings.resetSection`

## 4. Query 목록

- `query.listProjects`
- `query.getProject`
- `query.getWorkspace`
- `query.getTab`
- `query.getSession`
- `query.listSessionsByProject`
- `query.getSettings`
- `query.getCommandPaletteIndex`

## 5. Event 목록

### Session Events

- `event.sessionCreated`
- `event.sessionStarted`
- `event.sessionOutputChunk`
- `event.sessionExit`
- `event.sessionFailed`
- `event.sessionTitleChanged`

### Workspace Events

- `event.workspaceLoaded`
- `event.workspaceUpdated`
- `event.tabCreated`
- `event.tabClosed`
- `event.layoutChanged`
- `event.focusChanged`

### Project Events

- `event.projectCreated`
- `event.projectUpdated`
- `event.projectDeleted`
- `event.projectReordered`
- `event.activeProjectChanged`

### Settings Events

- `event.settingsUpdated`

## 6. Payload 예시

### session.create

```ts
type SessionCreateCommand = {
  projectId: string;
  shell: string;
  cwd?: string;
  titleHint?: string;
  envOverrides?: Record<string, string>;
};
```

### layout.splitPane

```ts
type LayoutSplitPaneCommand = {
  workspaceId: string;
  tabId: string;
  targetPaneId: string;
  direction: "horizontal" | "vertical";
  newPaneMode: "empty" | "new-session" | "attach-session";
  sessionCreateArgs?: SessionCreateCommand;
  existingSessionId?: string;
};
```

### event.sessionOutputChunk

```ts
type SessionOutputChunkEvent = {
  sessionId: string;
  chunk: string;
  receivedAt: string;
};
```

## 7. 상태 동기화 전략

모든 변화를 event로만 재구성하려고 하면 복잡해진다. 따라서 다음 혼합 전략을 쓴다.

- 큰 상태 로딩: query
- 세밀한 실시간 변화: event

예:

1. 프로젝트 전환 시 `query.getWorkspace`
2. 이후 변경은 `event.layoutChanged`, `event.sessionOutputChunk` 등으로 반영

## 8. 명령 처리 규칙

- 모든 command는 성공/실패를 명시적으로 반환한다.
- 실패는 사용자 표시용 메시지와 로그용 상세를 분리한다.
- 상태 저장이 필요한 command는 transaction 경계를 명확히 한다.

예:

- `layout.splitPane`는 레이아웃 변경과 세션 생성이 동시에 일어날 수 있다.
- 이 경우 "새 pane만 생기고 세션 생성은 실패" 같은 중간 상태 처리를 명확히 정의해야 한다.

권장:

- 세션 생성 실패 시 pane은 empty 상태로 남기고 에러 배너를 띄운다.

## 9. 이벤트 스트리밍 정책

`sessionOutputChunk`는 매우 빈번할 수 있으므로 주의가 필요하다.

정책:

- chunk는 일정량까지 배치 가능
- UI는 view가 보이지 않는 session에 대해 렌더링은 늦출 수 있음
- core는 output 전달과 persistence를 분리

## 10. IPC 버전 관리

장기적으로 command/event 계약이 늘어날 가능성이 높다.

권장 규칙:

- 공통 envelope에 `version` 필드 포함
- breaking change 시 command namespace 분리 가능
- contracts 패키지에서 타입 단일 관리

## 11. 오류 모델

오류는 세 가지로 나눈다.

- validation error
- runtime error
- infrastructure error

예:

- validation: 없는 projectId
- runtime: pane split 대상 없음
- infrastructure: ConPTY 생성 실패

UI는 오류 종류에 따라 다른 UX를 제공해야 한다.

- validation: 즉시 수정 유도
- runtime: 재시도 가능 안내
- infrastructure: 로그/진단 링크 제공
