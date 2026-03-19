import type { SessionOutputEvent, WorkspaceChangedEvent } from "../../../../packages/contracts/src/index.ts";

import { isTauriRuntime } from "./runtime";

export type WindowFileDropEvent = {
  type: "enter" | "over" | "drop" | "leave";
  paths: string[];
  position: {
    x: number;
    y: number;
  } | null;
};

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

export async function listenWorkspaceChanged(
  handler: (event: { payload: WorkspaceChangedEvent }) => void,
) {
  if (!isTauriRuntime()) {
    return () => {};
  }

  const { listen } = await import("@tauri-apps/api/event");
  return listen<WorkspaceChangedEvent>("workspace-changed", handler);
}

export async function listenWindowFileDrop(handler: (event: WindowFileDropEvent) => void) {
  if (!isTauriRuntime()) {
    return () => {};
  }

  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  return getCurrentWebview().onDragDropEvent((event) => {
    handler({
      type: event.payload.type,
      paths: "paths" in event.payload ? event.payload.paths : [],
      position: "position" in event.payload
        ? {
          x: event.payload.position.x,
          y: event.payload.position.y,
        }
        : null,
    });
  });
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
