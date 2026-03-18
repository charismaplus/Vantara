# Repository Structure and Standards

## 1. 목표

이번 프로젝트는 처음부터 모듈화를 전제로 시작해야 한다. "파일 수를 적게 유지"하는 것이 목표가 아니라, "책임이 명확한 구조를 유지"하는 것이 목표다.

## 2. 권장 리포지토리 구조

```text
repo/
  docs/
  apps/
    desktop/
      src-tauri/
        Cargo.toml
        capabilities/
        src/
          main.rs
          commands/
          bridges/
    ui/
      package.json
      src/
        app/
        features/
          projects/
          workspace/
          layout/
          terminal/
          settings/
          command-palette/
        shared/
          contracts/
          hooks/
          lib/
          store/
          ui/
  crates/
    core/
      src/
        project_manager/
        workspace_manager/
        layout_manager/
        session_manager/
        event_bus/
    pty/
      src/
        conpty/
        shell_profiles/
    persistence/
      src/
        sqlite/
        migrations/
    renderer_api/
      src/
        adapter.rs
  packages/
    contracts/
      src/
        commands.ts
        events.ts
        models.ts
```

## 3. 프런트엔드 구조 원칙

### 3.1 feature-first

프런트는 페이지 기준보다 기능 기준으로 나눈다.

예:

- `features/projects`
- `features/workspace`
- `features/layout`
- `features/terminal`

### 3.2 shared 분리

공통 유틸, UI primitive, 훅, 타입을 `shared`로 둔다.

하지만 shared가 쓰레기통이 되지 않도록 아래 규칙을 둔다.

- 두 feature 이상에서 재사용되기 전에는 shared로 올리지 않는다.
- 도메인 지식이 있는 코드는 shared로 보내지 않는다.

## 4. Rust 구조 원칙

Rust는 "기능별 service + manager" 구조를 쓴다.

예:

- `ProjectManager`
- `WorkspaceManager`
- `LayoutManager`
- `SessionManager`

규칙:

- manager는 공개 API를 가진다
- 세부 구현은 하위 모듈로 숨긴다
- cross-manager 호출은 facade 또는 app service를 통해 조정한다

## 5. 타입 공유 원칙

contracts는 별도 패키지에서 관리한다.

공유 대상:

- command payload
- event payload
- domain DTO
- layout snapshot schema

이유:

- UI와 core 간 타입 드리프트 방지
- 마이그레이션 추적 용이

## 6. 코드 스타일 원칙

### TypeScript

- strict mode 필수
- `any` 금지
- side effect를 feature entry에만 허용
- UI state와 server state를 분리

### Rust

- module public surface 최소화
- `unwrap`는 테스트 외 금지
- 에러 타입 명시
- 로그와 사용자 메시지 분리

## 7. 테스트 전략

이 프로젝트는 "복잡하지 않아서 테스트를 나중에"가 아니라, 구조가 명확하므로 테스트 포인트를 선명하게 잡아야 한다.

### 7.1 Unit Test

- layout tree reducer
- snapshot validation
- path validation
- session registry state transitions

### 7.2 Integration Test

- project create/open/restore
- tab create/split/close
- session create/write/resize/terminate
- snapshot persistence and restore

### 7.3 E2E Test

- 앱 실행
- 프로젝트 등록
- 탭과 split 생성
- 세션 출력 확인
- 앱 재실행 후 레이아웃 복원

## 8. 품질 게이트

머지 전 필수:

- 타입 검사 통과
- lint 통과
- 단위 테스트 통과
- 핵심 integration 테스트 통과
- snapshot schema 변경 시 migration 문서 갱신

## 9. 문서 운영 규칙

- 아키텍처 변경은 먼저 `docs/`를 수정한다.
- 새 도메인 개념이 생기면 해당 문서에 정의를 추가한다.
- 구현이 문서와 어긋나면 둘 중 하나를 즉시 정리한다.

## 10. 브랜치와 작업 단위

작업은 기능 단위보다 "계층 단위"로 자르는 것이 안정적이다.

예:

- core session lifecycle
- workspace persistence
- layout tree interactions
- terminal renderer adapter

이 방식이 UI와 core를 동시에 조금씩 건드리는 대형 충돌을 줄인다.

## 11. 금지 패턴

이번 코드베이스에서는 다음을 금지한다.

- 단일 거대 파일에 기능 누적
- UI에서 직접 shell 명령 실행
- renderer가 core 상태를 직접 소유
- 제품 규칙을 임의 문자열 prompt나 hardcoded UI 문구에 숨김
- 특정 AI provider 이름을 도메인 중심부에 박아두는 것

## 12. 구현 우선순위의 기준

구현은 "눈에 보이는 기능"보다 "나중에 다시 뜯지 않을 구조"를 우선한다.

우선순위:

1. contracts
2. core domain and managers
3. persistence
4. session runtime
5. renderer adapter
6. workspace UI
7. command palette and polish

이 순서를 지키면 제품이 커져도 구조가 덜 무너진다.
