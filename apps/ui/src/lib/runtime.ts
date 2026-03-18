declare global {
  interface Window {
    __TAURI__?: unknown;
    __TAURI_INTERNALS__?: unknown;
  }
}

export function isTauriRuntime() {
  if (typeof window === "undefined") {
    return false;
  }

  return Boolean(window.__TAURI__ || window.__TAURI_INTERNALS__);
}
