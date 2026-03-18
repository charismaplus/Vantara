import type { FitAddon } from "xterm-addon-fit";
import type { Terminal } from "xterm";

type Entry = {
  term: Terminal;
  fit: FitAddon;
};

const terminals = new Map<string, Entry>();
const pendingChunks = new Map<string, string[]>();

export function registerTerminal(sessionId: string, entry: Entry) {
  terminals.set(sessionId, entry);
  const buffered = pendingChunks.get(sessionId);
  if (!buffered) {
    return;
  }

  for (const chunk of buffered) {
    entry.term.write(chunk);
  }
  pendingChunks.delete(sessionId);
}

export function unregisterTerminal(sessionId: string) {
  terminals.delete(sessionId);
}

export function writeTerminalChunk(sessionId: string, chunk: string) {
  const entry = terminals.get(sessionId);
  if (entry) {
    entry.term.write(chunk);
    return;
  }

  const chunks = pendingChunks.get(sessionId) ?? [];
  chunks.push(chunk);
  pendingChunks.set(sessionId, chunks.slice(-200));
}

export function fitTerminal(sessionId: string) {
  terminals.get(sessionId)?.fit.fit();
}

export function pasteTerminalInput(sessionId: string, text: string) {
  const entry = terminals.get(sessionId);
  if (!entry || !text) {
    return false;
  }

  entry.term.paste(text);
  return true;
}
