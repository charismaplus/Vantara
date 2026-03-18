import type { SessionOutputEvent } from "@workspace-terminal/contracts";

import { isTauriRuntime } from "./runtime";

export async function listenSessionOutput(
  handler: (event: { payload: SessionOutputEvent }) => void,
) {
  if (!isTauriRuntime()) {
    return () => {};
  }

  const { listen } = await import("@tauri-apps/api/event");
  return listen<SessionOutputEvent>("session-output", handler);
}

export async function listenSessionExit(handler: () => void) {
  if (!isTauriRuntime()) {
    return () => {};
  }

  const { listen } = await import("@tauri-apps/api/event");
  return listen("session-exit", handler);
}

export async function openDirectoryDialog() {
  if (!isTauriRuntime()) {
    const value = window.prompt("Project path");
    return value?.trim() ? value.trim() : null;
  }

  const { open } = await import("@tauri-apps/plugin-dialog");
  const selected = await open({
    directory: true,
    multiple: false,
    title: "Choose project folder",
  });

  return typeof selected === "string" ? selected : null;
}
