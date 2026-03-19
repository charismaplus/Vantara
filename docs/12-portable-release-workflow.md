# Portable Release Workflow

포터블 배포 검증 대상은 아래 경로 하나로 고정한다.

- `target/release/bundle/portable/Workspace Terminal Portable/Workspace Terminal Portable.exe`

루트 경로의 `Workspace Terminal Portable.exe`는 잔존 보조 파일로 취급하며, 배포 검증 기준에 포함하지 않는다.

## 표준 실행 명령

리포지토리 루트에서 아래 명령만 사용한다.

```powershell
npm run portable:refresh
```

위 명령은 다음 순서를 강제한다.

1. `cargo build --manifest-path apps/desktop/src-tauri/Cargo.toml --release --bin tmux`
2. `WORKSPACE_TERMINAL_EMBED_TMUX_PATH=target/release/tmux.exe` 환경변수로 앱 exe 빌드
3. `target/release/workspace-terminal-desktop.exe`를 포터블 경로로 덮어쓰기
4. 기존 포터블 폴더 안의 `shim/` 잔존 디렉터리 제거
5. 앱 exe 원본/포터블 SHA256 일치 여부 검증

## 실패 처리 규칙

- 포터블 exe가 실행 중이거나 잠겨 있으면 즉시 실패한다.
- 복사 후 SHA256이 다르면 즉시 실패한다.
- 실패 시 배포 성공으로 간주하지 않는다.

## 확인 기준

성공 시 아래 항목이 출력되어야 한다.

- source 경로
- portable 경로
- SHA256
- embedded tmux source 경로
- 파일 크기
- source/portable 수정 시각

## 전달물 기준

- 사용자에게 전달하는 포터블 결과물은 `Workspace Terminal Portable.exe` 단일 파일이다.
- 포터블 폴더 안에 `shim/` 디렉터리는 남지 않아야 한다.
- tmux helper는 앱 실행 중 `%TEMP%\WorkspaceTerminal\<hash>\tmux.exe`로 자동 복구된다.
