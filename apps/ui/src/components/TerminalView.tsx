import { useEffect, useRef } from "react";
import "xterm/css/xterm.css";

import { readClipboardPayload, resizeSession, writeSessionInput } from "../lib/api";
import { isTauriRuntime } from "../lib/runtime";
import { registerTerminal, unregisterTerminal } from "../lib/terminalRegistry";

type TerminalViewProps = {
  sessionId: string;
  variant?: "user" | "ai";
};

export function TerminalView({ sessionId, variant = "user" }: TerminalViewProps) {
  const hostRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) {
      return;
    }

    let observer: ResizeObserver | null = null;
    let disposeData: { dispose: () => void } | null = null;
    let mounted = true;
    let terminalDispose: (() => void) | null = null;
    let resizeFrame: number | null = null;
    let lastHostRect = { width: 0, height: 0 };
    let lastSentSize = { cols: 0, rows: 0 };
    let firstOutputLogged = false;

    void (async () => {
      const [{ Terminal }, { FitAddon }] = await Promise.all([import("xterm"), import("xterm-addon-fit")]);
      if (!mounted) {
        return;
      }

      const terminal = new Terminal({
        fontFamily: "Consolas, 'Cascadia Code', monospace",
        fontSize: 13,
        lineHeight: 1.1,
        scrollback: 5000,
        theme: {
          background: variant === "ai" ? "#071917" : "#08111f",
          foreground: "#dbeafe",
          cursor: variant === "ai" ? "#31d4a7" : "#38bdf8",
          black: "#0f172a",
          blue: "#60a5fa",
          green: "#34d399",
        },
      });
      const fit = new FitAddon();
      terminal.loadAddon(fit);
      terminal.open(host);
      console.debug("[terminal] mount", { sessionId, variant });

      terminal.attachCustomKeyEventHandler((event) => {
        if (event.type !== "keydown") {
          return true;
        }

        const isCopy = (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "c";
        if (isCopy && terminal.hasSelection()) {
          const selection = terminal.getSelection();
          if (selection) {
            void navigator.clipboard.writeText(selection).catch(() => {});
          }
          terminal.clearSelection();
          event.preventDefault();
          return false;
        }

        const isPaste = (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "v";
        if (isPaste) {
          event.preventDefault();
          void readClipboardPayload()
            .then((payload) => {
              if (payload.kind === "files" && payload.paths.length) {
                terminal.paste(payload.paths.join(" "));
                return;
              }
              if (payload.kind === "imagePath" && payload.imagePath) {
                terminal.paste(payload.imagePath);
                return;
              }
              if (payload.kind === "text" && payload.text) {
                terminal.paste(payload.text);
              }
            })
            .catch(() => {});
          return false;
        }

        return true;
      });

      const scheduleFit = (reason: "mount" | "resize") => {
        if (!mounted) {
          return;
        }

        if (resizeFrame !== null) {
          cancelAnimationFrame(resizeFrame);
        }

        resizeFrame = requestAnimationFrame(() => {
          resizeFrame = null;

          if (!mounted) {
            return;
          }

          const rect = host.getBoundingClientRect();
          const width = Math.round(rect.width);
          const height = Math.round(rect.height);
          const rectChanged = width !== lastHostRect.width || height !== lastHostRect.height;

          if (reason === "resize" && !rectChanged) {
            return;
          }

          if (width <= 0 || height <= 0) {
            return;
          }

          lastHostRect = { width, height };
          fit.fit();

          console.debug(`[terminal] ${reason === "mount" ? "first fit" : "fit"}`, {
            sessionId,
            width,
            height,
            cols: terminal.cols,
            rows: terminal.rows,
          });

          if (terminal.cols <= 0 || terminal.rows <= 0) {
            return;
          }

          const colsChanged = terminal.cols !== lastSentSize.cols;
          const rowsChanged = terminal.rows !== lastSentSize.rows;
          if (!colsChanged && !rowsChanged) {
            return;
          }

          lastSentSize = { cols: terminal.cols, rows: terminal.rows };
          console.debug("[terminal] resize_session", {
            sessionId,
            cols: terminal.cols,
            rows: terminal.rows,
          });
          void resizeSession(sessionId, terminal.cols, terminal.rows);
        });
      };

      scheduleFit("mount");

      if (!isTauriRuntime()) {
        terminal.writeln("Vantara Mock Preview");
        terminal.writeln("");
        terminal.writeln("This browser preview renders the layout without Tauri PTY sessions.");
        terminal.writeln("Use the split controls and project navigation to inspect the UI.");
        terminal.writeln("");
      }

      registerTerminal(sessionId, { term: terminal, fit });

      disposeData = terminal.onData((input) => {
        void writeSessionInput(sessionId, input);
      });

      observer = new ResizeObserver(() => {
        scheduleFit("resize");
      });
      observer.observe(host);

      const handleDragOver = (event: DragEvent) => {
        event.preventDefault();
        if (event.dataTransfer) {
          event.dataTransfer.dropEffect = "copy";
        }
      };

      const handleDrop = (event: DragEvent) => {
        event.preventDefault();

        const droppedText = event.dataTransfer?.getData("text/plain")?.trim();
        if (droppedText) {
          terminal.paste(droppedText);
          return;
        }

        const files = Array.from(event.dataTransfer?.files ?? []);
        if (!files.length || isTauriRuntime()) {
          return;
        }

        const fallbackNames = files.map((file) => file.name).filter(Boolean).join(" ");
        if (fallbackNames) {
          terminal.paste(fallbackNames);
        }
      };

      host.addEventListener("dragover", handleDragOver);
      host.addEventListener("drop", handleDrop);

      const originalWrite = terminal.write.bind(terminal);
      terminal.write = ((data: string | Uint8Array, callback?: () => void) => {
        if (!firstOutputLogged) {
          firstOutputLogged = true;
          console.debug("[terminal] first output", { sessionId });
        }
        return originalWrite(data, callback);
      }) as typeof terminal.write;

      terminalDispose = () => {
        if (resizeFrame !== null) {
          cancelAnimationFrame(resizeFrame);
        }
        observer?.disconnect();
        disposeData?.dispose();
        host.removeEventListener("dragover", handleDragOver);
        host.removeEventListener("drop", handleDrop);
        unregisterTerminal(sessionId);
        terminal.dispose();
      };
    })();

    return () => {
      mounted = false;
      if (resizeFrame !== null) {
        cancelAnimationFrame(resizeFrame);
      }
      terminalDispose?.();
      observer?.disconnect();
      disposeData?.dispose();
      unregisterTerminal(sessionId);
    };
  }, [sessionId, variant]);

  return (
    <div
      className={`terminal-host ${variant === "ai" ? "ai-terminal" : "user-terminal"}`}
      data-terminal-session-id={sessionId}
      ref={hostRef}
    />
  );
}
