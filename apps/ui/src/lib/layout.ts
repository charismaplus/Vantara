import type { LayoutNode, WorkspaceTab } from "../../../../packages/contracts/src/index.ts";

export function getActiveTab(tabs: WorkspaceTab[], activeTabId?: string | null) {
  return tabs.find((tab) => tab.id === activeTabId) ?? tabs[0] ?? null;
}

export function isStackNode(node: LayoutNode) {
  return node.type === "stack";
}

export function isSplitNode(node: LayoutNode) {
  return node.type === "split";
}
