# Vantara

[English README](./README.en.md)

> 이 프로젝트는 프로그래머가 아닌 기획자가 바이브 코딩으로 작성한 프로젝트입니다.

Vantara는 AI CLI 기반 개발을 위해 만든 프로젝트 중심 데스크톱 터미널 워크스페이스입니다.  
일반적인 터미널처럼 탭만 늘어놓는 대신, 프로젝트와 세션, 탭, pane을 기준으로 작업 문맥을 정리하는 데 초점을 둡니다.

## 한눈에 보기

- 프로젝트별 세션 트리를 좌측 사이드바에서 관리
- 세션 내부에서 상단 탭과 split pane으로 작업 공간 구성
- `Claude Code`, `Claude Unsafe`, `Codex`, `Codex Full Auto`, `Terminal` 런처 제공
- tmux shim을 통해 AI CLI의 `split-window`, `new-window` 같은 흐름을 앱 레이아웃에 연결
- 드래그 앤 드롭, 클립보드 붙여넣기, 상태 패널 등 개발 중심 UX 제공
- Windows용 포터블 실행 파일 제공

## 작업 구조

Vantara는 아래 구조를 기준으로 동작합니다.

- `Project`  
  코드베이스 단위의 최상위 컨테이너
- `Session`  
  프로젝트 아래의 작업 묶음
- `Tab`  
  세션 내부 상단 탭
- `Pane`  
  탭 내부의 split 영역

즉, 여러 프로젝트를 오가면서도 각 프로젝트 안의 세션과 탭, pane 구조를 분리해서 볼 수 있습니다.

## 현재 기술 스택

- Tauri 2
- React 19
- Vite
- TypeScript
- Rust
- SQLite
- `portable-pty`
- `xterm.js`

참고로 현재 터미널 렌더러는 `xterm.js` 기반입니다.

## 지원 환경

- Windows 우선
- Node.js
- Rust toolchain

선택적으로 아래 CLI가 설치되어 있으면 런처에서 바로 사용할 수 있습니다.

- `claude`
- `codex`

Windows에서 tmux 기반 child pane 흐름을 더 안정적으로 쓰려면 Git Bash 설치를 권장합니다.

## 빠른 시작

의존성 설치:

```powershell
npm install
```

웹 UI 개발 실행:

```powershell
npm run dev
```

데스크톱 앱 개발 실행:

```powershell
npm run tauri:dev
```

프런트엔드 빌드:

```powershell
npm run build
```

포터블 빌드 갱신:

```powershell
npm run portable:refresh
```

공식 포터블 산출물 경로:

```text
target/release/bundle/portable/Vantara Portable/Vantara Portable.exe
```

## 저장소 구조

- `apps/ui`
  React 기반 UI
- `apps/desktop/src-tauri`
  Tauri + Rust 네이티브 런타임
- `packages/contracts`
  프런트/백엔드 공유 계약 타입
- `devhub_src`
  과거 참고 자료

## 현재 상태

Vantara는 이미 Windows 데스크톱 워크스페이스 터미널로 사용할 수 있는 상태지만, 여전히 활발하게 개선 중입니다.

현재 중점 영역:

- 프로젝트/세션 사용성
- pane 및 탭 조작 UX
- tmux shim 호환성
- PTY 안정성
- 터미널 렌더링 및 성능 개선

## 참고

- 이 저장소에서는 내부 설계 문서와 실험 문서를 Git 커밋 대상에서 제외합니다.
- 실제 제품 코드는 `apps/`와 `packages/` 아래를 기준으로 봐야 합니다.
