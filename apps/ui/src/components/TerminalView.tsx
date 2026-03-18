import { useEffect, useRef } from "react";
import "xterm/css/xterm.css";

import { resizeSession, writeSessionInput } from "../lib/api";
import { isTauriRuntime } from "../lib/runtime";
import { fitTerminal, registerTerminal, unregisterTerminal } from "../lib/terminalRegistry";

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
      requestAnimationFrame(() => {
        fit.fit();
        void resizeSession(sessionId, terminal.cols, terminal.rows);
      });

      if (!isTauriRuntime()) {
        terminal.writeln("Workspace Terminal Mock Preview");
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
        requestAnimationFrame(() => {
          fitTerminal(sessionId);
          void resizeSession(sessionId, terminal.cols, terminal.rows);
        });
      });
      observer.observe(host);

      terminalDispose = () => {
        observer?.disconnect();
        disposeData?.dispose();
        unregisterTerminal(sessionId);
        terminal.dispose();
      };
    })();

    return () => {
      mounted = false;
      terminalDispose?.();
      observer?.disconnect();
      disposeData?.dispose();
      unregisterTerminal(sessionId);
    };
  }, [sessionId, variant]);

  return <div className={`terminal-host ${variant === "ai" ? "ai-terminal" : "user-terminal"}`} ref={hostRef} />;
}
