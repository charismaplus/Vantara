# Workspace Terminal Product Docs

이 폴더는 기존 DevHub를 직접 확장하는 문서가 아니라, 새로 설계할 범용 프로젝트 워크스페이스 터미널 제품의 기준 문서 세트다.

제품 목표는 다음과 같다.

- 여러 프로젝트를 메신저처럼 빠르게 전환할 수 있다.
- 각 프로젝트 안에서 상단 탭과 split-pane 레이아웃으로 터미널 세션을 운영할 수 있다.
- 특정 AI CLI에 종속되지 않는 범용 터미널 플랫폼으로 설계한다.
- UI는 웹 기술로 구현하되, 터미널 렌더링과 PTY 수명주기는 네이티브 코어에서 담당한다.
- 터미널 엔진은 장기적으로 Ghostty 계열 네이티브 렌더러를 목표로 하며, 구조적으로 렌더러 교체가 가능해야 한다.

문서 목록:

1. [01-product-definition.md](./01-product-definition.md)
   제품 목표, 사용자 시나리오, 핵심 UX, 제외 범위
2. [02-system-architecture.md](./02-system-architecture.md)
   전체 시스템 아키텍처, 런타임 분리, 모듈 경계
3. [03-domain-model.md](./03-domain-model.md)
   프로젝트, 워크스페이스, 탭, pane, 세션 등 핵심 도메인 모델
4. [04-ui-workspace-layout.md](./04-ui-workspace-layout.md)
   데스크톱 UI 구조, 레이아웃 트리, 상호작용 규칙
5. [05-terminal-runtime-and-renderer.md](./05-terminal-runtime-and-renderer.md)
   PTY 런타임, terminal adapter, Ghostty 통합 전략
6. [06-ipc-events-and-commands.md](./06-ipc-events-and-commands.md)
   UI와 코어 간 명령, 이벤트, 상태 동기화 계약
7. [07-data-storage-and-state.md](./07-data-storage-and-state.md)
   저장소, 스냅샷, 세션 상태, SQLite 스키마
8. [08-security-permissions-and-reliability.md](./08-security-permissions-and-reliability.md)
   권한 모델, 보안 경계, 충돌 방지, 안정성 설계
9. [09-repository-structure-and-standards.md](./09-repository-structure-and-standards.md)
   코드베이스 구조, 개발 규칙, 품질 기준, 테스트 전략
10. [10-delivery-workstreams.md](./10-delivery-workstreams.md)
   구현 작업 스트림, 선행 관계, 제품 완료 기준

문서 사용 원칙:

- 이 문서들은 "MVP용 축약본"이 아니라 완전한 제품을 만드는 기준선이다.
- 구현 중 결정이 필요할 때는 먼저 이 문서를 갱신한 뒤 코드를 변경한다.
- 특정 기술 선택이 바뀌어도 도메인 모델과 모듈 경계는 최대한 유지한다.
- 기존 DevHub의 개념은 참고만 하며, 구조는 새 문서를 기준으로 재구성한다.
