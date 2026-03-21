import type { MouseEvent as ReactMouseEvent, PointerEvent as ReactPointerEvent, ReactNode } from "react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Group as PanelGroup, Panel, Separator as PanelResizeHandle } from "react-resizable-panels";
import type {
  LayoutNode,
  LaunchProfile,
  PaneMovePlacement,
  Project,
  ProjectWorkspaceSnapshot,
  SessionSidebarStatus,
  TerminalSession,
  WorkspaceSession,
  WorkspaceSnapshot,
  WorkspaceWindow,
} from "../../../packages/contracts/src/index.ts";

import { AppErrorBoundary } from "./components/AppErrorBoundary";
import { TerminalView } from "./components/TerminalView";
import {
  closePane,
  closeWindow,
  createProject,
  createSession,
  createWindow,
  createWorkspaceSession,
  deleteProject,
  deleteWorkspaceSession,
  getSessionSidebarStatus,
  listProjects,
  movePane,
  openProject,
  openSession,
  renameProject,
  renameWindow,
  renameWorkspaceSession,
  reportTabViewport,
  setActivePane,
  setActiveWindow,
  splitPane,
} from "./lib/api";
import {
  listenSessionExit,
  listenSessionOutput,
  listenSessionStatusChanged,
  listenWindowFileDrop,
  listenWorkspaceChanged,
  openDirectoryDialog,
} from "./lib/desktop";
import { getActiveTab, isSplitNode, isStackNode } from "./lib/layout";
import { pasteTerminalInput, writeTerminalChunk } from "./lib/terminalRegistry";
import vantaraLogo from "../../../docs/Logo.png";

type LauncherProfile = {
  id: string;
  title: string;
  description: string;
  program: string;
  args?: string[];
  launchProfile: LaunchProfile;
  theme: "claude" | "claude-danger" | "codex" | "codex-auto" | "terminal";
  sessionTitle: string;
};

type SidebarContextMenuState =
  | {
      kind: "project";
      project: Project;
      x: number;
      y: number;
    }
  | {
      kind: "session";
      project: Project;
      session: WorkspaceSession;
      x: number;
      y: number;
    };

type PaneMoveMenuState = {
  paneId: string;
  x: number;
  y: number;
};

type DirectionalPaneMovePlacement = Exclude<PaneMovePlacement, "swap">;

const launcherRows: LauncherProfile[][] = [
  [
    { id: "claude", title: "Claude Code", description: "Interactive coding session", program: "claude", launchProfile: "claude", theme: "claude", sessionTitle: "Claude Code" },
    { id: "claude-unsafe", title: "Claude Unsafe", description: "Bypass permissions", program: "claude", args: ["--dangerously-skip-permissions"], launchProfile: "claudeUnsafe", theme: "claude-danger", sessionTitle: "Claude Unsafe" },
  ],
  [
    { id: "codex", title: "Codex", description: "Interactive agent session", program: "codex", launchProfile: "codex", theme: "codex", sessionTitle: "Codex" },
    { id: "codex-full-auto", title: "Codex Full Auto", description: "Workspace-write auto mode", program: "codex", args: ["--full-auto"], launchProfile: "codexFullAuto", theme: "codex-auto", sessionTitle: "Codex Full Auto" },
  ],
  [
    { id: "terminal", title: "Terminal", description: "Plain PowerShell shell", program: "powershell", launchProfile: "terminal", theme: "terminal", sessionTitle: "PowerShell" },
  ],
];

function createSessionMap(snapshot: WorkspaceSnapshot | null) {
  return new Map<string, TerminalSession>(snapshot?.terminals.map((session) => [session.id, session]) ?? []);
}

function getMinimumPanelSize(direction: "horizontal" | "vertical") {
  return direction === "vertical" ? 24 : 20;
}

function createDefaultSplitLayout(node: Extract<LayoutNode, { type: "split" }>) {
  return Object.fromEntries(node.children.map((child, index) => [child.id, node.sizes[index] ?? 50]));
}

function getResolvedSplitLayout(node: Extract<LayoutNode, { type: "split" }>, savedLayout?: Record<string, number>) {
  const fallbackLayout = createDefaultSplitLayout(node);
  if (!savedLayout) {
    return fallbackLayout;
  }
  const childIds = node.children.map((child) => child.id);
  const hasAllChildren = childIds.every((id) => typeof savedLayout[id] === "number");
  return hasAllChildren && Object.keys(savedLayout).length === childIds.length ? savedLayout : fallbackLayout;
}

function collectPaneLabels(node: LayoutNode, paneLabels: Map<string, string>) {
  if (isStackNode(node)) {
    paneLabels.set(node.id, node.paneLabel);
    return;
  }
  if (isSplitNode(node)) {
    for (const child of node.children) {
      collectPaneLabels(child, paneLabels);
    }
  }
}

function findStackNode(node: LayoutNode, targetId: string): Extract<LayoutNode, { type: "stack" }> | null {
  if (isStackNode(node)) {
    return node.id === targetId ? node : null;
  }
  for (const child of node.children) {
    const match = findStackNode(child, targetId);
    if (match) {
      return match;
    }
  }
  return null;
}

function getActiveTerminalForPane(snapshot: WorkspaceSnapshot | null, window: WorkspaceWindow | null) {
  if (!snapshot || !window) {
    return null;
  }
  const paneId = window.activePaneId ?? null;
  if (!paneId) {
    return null;
  }
  const stack = findStackNode(window.root, paneId);
  if (!stack) {
    return null;
  }
  const activeItem = stack.items.find((item) => item.id === stack.activeItemId) ?? stack.items[0];
  if (!activeItem?.sessionId) {
    return null;
  }
  return snapshot.terminals.find((session) => session.id === activeItem.sessionId) ?? null;
}

function resolveDropPlacementFromRect(rect: DOMRect, clientX: number, clientY: number): PaneMovePlacement {
  const x = clientX - rect.left;
  const y = clientY - rect.top;
  const horizontalInset = rect.width * 0.24;
  const verticalInset = rect.height * 0.24;

  if (y <= verticalInset) {
    return "top";
  }
  if (y >= rect.height - verticalInset) {
    return "bottom";
  }
  if (x <= horizontalInset) {
    return "left";
  }
  if (x >= rect.width - horizontalInset) {
    return "right";
  }
  return "swap";
}

function withTimeout<T>(promise: Promise<T>, timeoutMs: number, timeoutMessage: string) {
  let timeoutId: number | null = null;
  const timeoutPromise = new Promise<T>((_resolve, reject) => {
    timeoutId = window.setTimeout(() => reject(new Error(timeoutMessage)), timeoutMs);
  });
  return Promise.race([promise, timeoutPromise]).finally(() => {
    if (timeoutId !== null) {
      window.clearTimeout(timeoutId);
    }
  });
}

function getErrorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

export default function App() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [activeProjectId, setActiveProjectId] = useState<string | null>(null);
  const [projectSnapshotsById, setProjectSnapshotsById] = useState<Record<string, ProjectWorkspaceSnapshot>>({});
  const [activeWorkspaceSessionId, setActiveWorkspaceSessionId] = useState<string | null>(null);
  const [sessionSnapshot, setSessionSnapshot] = useState<WorkspaceSnapshot | null>(null);
  const [expandedProjectIds, setExpandedProjectIds] = useState<Record<string, boolean>>({});
  const [isProjectModalOpen, setIsProjectModalOpen] = useState(false);
  const [panelLayouts, setPanelLayouts] = useState<Record<string, Record<string, number>>>({});
  const [newProjectName, setNewProjectName] = useState("");
  const [newProjectPath, setNewProjectPath] = useState("");
  const [loading, setLoading] = useState(false);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [launchingPaneIds, setLaunchingPaneIds] = useState<string[]>([]);
  const [renamingProjectId, setRenamingProjectId] = useState<string | null>(null);
  const [renamingProjectName, setRenamingProjectName] = useState("");
  const [pendingDeleteProject, setPendingDeleteProject] = useState<Project | null>(null);
  const [pendingDeleteSession, setPendingDeleteSession] = useState<{ projectId: string; session: WorkspaceSession } | null>(null);
  const [sidebarContextMenu, setSidebarContextMenu] = useState<SidebarContextMenuState | null>(null);
  const [paneMoveMenu, setPaneMoveMenu] = useState<PaneMoveMenuState | null>(null);
  const [sidebarStatus, setSidebarStatus] = useState<SessionSidebarStatus | null>(null);
  const [draggingPaneId, setDraggingPaneId] = useState<string | null>(null);
  const [dragTarget, setDragTarget] = useState<{ paneId: string; placement: PaneMovePlacement } | null>(null);
  const workspaceCanvasRef = useRef<HTMLElement | null>(null);
  const activeProjectIdRef = useRef<string | null>(null);
  const activeWorkspaceSessionIdRef = useRef<string | null>(null);
  const activeTerminalSessionIdRef = useRef<string | null>(null);
  const draggingPaneIdRef = useRef<string | null>(null);
  const selectionRequestSeqRef = useRef(0);
  const workspaceMutationSeqRef = useRef(0);
  const dragPointerIdRef = useRef<number | null>(null);
  const dragHandleElementRef = useRef<HTMLElement | null>(null);
  const refreshProjectSummaryRef = useRef<(projectId: string) => Promise<void>>(async () => {});
  const refreshCurrentSelectionRef = useRef<() => Promise<void>>(async () => {});
  const movePaneWithinWindowRef = useRef<(sourcePaneId: string, targetPaneId: string, placement: PaneMovePlacement) => Promise<void>>(async () => {});

  const activeProject = useMemo(() => projects.find((entry) => entry.id === activeProjectId) ?? null, [projects, activeProjectId]);
  const activeProjectSnapshot = useMemo(
    () => (activeProjectId ? projectSnapshotsById[activeProjectId] ?? null : null),
    [activeProjectId, projectSnapshotsById],
  );
  const activeWorkspaceSession = useMemo(
    () => activeProjectSnapshot?.sessions.find((entry) => entry.id === activeWorkspaceSessionId) ?? null,
    [activeProjectSnapshot, activeWorkspaceSessionId],
  );
  const sessions = useMemo(() => createSessionMap(sessionSnapshot), [sessionSnapshot]);
  const activeWindow = useMemo(
    () => getActiveTab(sessionSnapshot?.windows ?? [], sessionSnapshot?.activeWindowId),
    [sessionSnapshot],
  );
  const activeTerminalSession = useMemo(
    () => getActiveTerminalForPane(sessionSnapshot, activeWindow),
    [sessionSnapshot, activeWindow],
  );
  const activeTerminalSessionId = activeTerminalSession?.id ?? null;
  const paneLabels = useMemo(() => {
    const labels = new Map<string, string>();
    if (activeWindow) {
      collectPaneLabels(activeWindow.root, labels);
    }
    return labels;
  }, [activeWindow]);

  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  useEffect(() => {
    activeWorkspaceSessionIdRef.current = activeWorkspaceSessionId;
  }, [activeWorkspaceSessionId]);

  useEffect(() => {
    draggingPaneIdRef.current = draggingPaneId;
  }, [draggingPaneId]);

  useEffect(() => {
    activeTerminalSessionIdRef.current = activeTerminalSessionId;
  }, [activeTerminalSessionId]);

  function mergeProjectSnapshot(nextSnapshot: ProjectWorkspaceSnapshot) {
    setProjectSnapshotsById((current) => ({ ...current, [nextSnapshot.projectId]: nextSnapshot }));
  }

  function isSelectionRequestCurrent(requestSeq: number, projectId: string, workspaceSessionId: string | null) {
    return selectionRequestSeqRef.current === requestSeq
      && activeProjectIdRef.current === projectId
      && (activeWorkspaceSessionIdRef.current ?? null) === (workspaceSessionId ?? null);
  }

  useEffect(() => {
    void (async () => {
      try {
        const loadedProjects = await listProjects();
        setProjects(loadedProjects);
        if (loadedProjects[0]) {
          const projectSnapshots = await Promise.all(
            loadedProjects.map(async (project) => [project.id, await openProject(project.id)] as const),
          );
          setProjectSnapshotsById(Object.fromEntries(projectSnapshots));
          setExpandedProjectIds({ [loadedProjects[0].id]: true });
          setLoading(true);
          activeProjectIdRef.current = loadedProjects[0].id;
          activeWorkspaceSessionIdRef.current = null;
          setActiveProjectId(loadedProjects[0].id);
          setActiveWorkspaceSessionId(null);
          setSessionSnapshot(null);
          setSidebarStatus(null);
          setLoading(false);
        }
      } catch (error) {
        showError(error);
        setLoading(false);
      }
    })();
  }, []);

  async function refreshProjectSummary(projectId: string) {
    try {
      const nextProjectSnapshot = await openProject(projectId);
      mergeProjectSnapshot(nextProjectSnapshot);
      setProjects(await listProjects());
    } catch (error) {
      showError(error);
    }
  }

  async function refreshCurrentSelection() {
    const projectId = activeProjectIdRef.current;
    if (!projectId) {
      return;
    }
    const requestSeq = selectionRequestSeqRef.current + 1;
    selectionRequestSeqRef.current = requestSeq;
    const expectedWorkspaceSessionId = activeWorkspaceSessionIdRef.current;

    setLoading(true);
    try {
      const [nextProjectSnapshot, nextProjects] = await Promise.all([
        openProject(projectId),
        listProjects(),
      ]);
      if (!isSelectionRequestCurrent(requestSeq, projectId, expectedWorkspaceSessionId)) {
        return;
      }
      mergeProjectSnapshot(nextProjectSnapshot);
      setProjects(nextProjects);

      if (
        expectedWorkspaceSessionId
        && nextProjectSnapshot.sessions.some((entry) => entry.id === expectedWorkspaceSessionId)
      ) {
        const nextSessionSnapshot = await openSession(projectId, expectedWorkspaceSessionId);
        if (!isSelectionRequestCurrent(requestSeq, projectId, expectedWorkspaceSessionId)) {
          return;
        }
        setSessionSnapshot(nextSessionSnapshot);
      } else {
        if (!isSelectionRequestCurrent(requestSeq, projectId, expectedWorkspaceSessionId)) {
          return;
        }
        if (expectedWorkspaceSessionId) {
          activeWorkspaceSessionIdRef.current = null;
          setActiveWorkspaceSessionId(null);
        }
        setSessionSnapshot(null);
      }
    } catch (error) {
      if (selectionRequestSeqRef.current === requestSeq) {
        showError(error);
      }
    } finally {
      if (selectionRequestSeqRef.current === requestSeq) {
        setLoading(false);
      }
    }
  }

  refreshProjectSummaryRef.current = refreshProjectSummary;
  refreshCurrentSelectionRef.current = refreshCurrentSelection;

  useEffect(() => {
    let disposed = false;
    let outputDispose: (() => void) | null = null;
    let exitDispose: (() => void) | null = null;
    let workspaceDispose: (() => void) | null = null;
    let statusDispose: (() => void) | null = null;

    const bindListener = (
      promise: Promise<() => void>,
      assign: (dispose: () => void) => void,
    ) => {
      void promise
        .then((dispose) => {
          if (disposed) {
            dispose();
            return;
          }
          assign(dispose);
        })
        .catch((error) => setErrorMessage(getErrorMessage(error)));
    };

    bindListener(
      listenSessionOutput((event) => writeTerminalChunk(event.payload.sessionId, event.payload.chunk)),
      (dispose) => {
        outputDispose = dispose;
      },
    );

    bindListener(
      listenSessionExit(() => {
        void refreshCurrentSelectionRef.current();
      }),
      (dispose) => {
        exitDispose = dispose;
      },
    );

    bindListener(
      listenWorkspaceChanged((event) => {
        void refreshProjectSummaryRef.current(event.payload.projectId);

        const activeProjectId = activeProjectIdRef.current;
        if (!activeProjectId || event.payload.projectId !== activeProjectId) {
          return;
        }

        void refreshCurrentSelectionRef.current();
      }),
      (dispose) => {
        workspaceDispose = dispose;
      },
    );

    bindListener(
      listenSessionStatusChanged((event) => {
        if (event.payload.sessionId === activeTerminalSessionIdRef.current) {
          setSidebarStatus(event.payload);
        }
      }),
      (dispose) => {
        statusDispose = dispose;
      },
    );

    return () => {
      disposed = true;
      outputDispose?.();
      exitDispose?.();
      workspaceDispose?.();
      statusDispose?.();
    };
  }, []);

  useEffect(() => {
    const element = workspaceCanvasRef.current;
    if (!element || !activeProjectId || !activeWindow) {
      return;
    }
    let frame: number | null = null;
    let lastWidth = 0;
    let lastHeight = 0;
    const observer = new ResizeObserver(() => {
      if (frame !== null) {
        cancelAnimationFrame(frame);
      }
      frame = requestAnimationFrame(() => {
        frame = null;
        const rect = element.getBoundingClientRect();
        const width = Math.round(rect.width);
        const height = Math.round(rect.height);
        if (width <= 0 || height <= 0 || (width === lastWidth && height === lastHeight)) {
          return;
        }
        lastWidth = width;
        lastHeight = height;
        void reportTabViewport(activeProjectId, activeWindow.id, width, height).catch(showError);
      });
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
      if (frame !== null) {
        cancelAnimationFrame(frame);
      }
    };
  }, [activeProjectId, activeWindow]);

  useEffect(() => {
    if (!activeTerminalSessionId) {
      setSidebarStatus(null);
      return;
    }

    let cancelled = false;
    void getSessionSidebarStatus(activeTerminalSessionId)
      .then((status) => {
        if (!cancelled) {
          setSidebarStatus(status);
        }
      })
      .catch((error) => {
        if (!cancelled) {
          showError(error);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [activeTerminalSessionId]);

  useEffect(() => {
    let disposed = false;
    let dispose: (() => void) | null = null;

    void listenWindowFileDrop((event) => {
      if (event.type !== "drop" || !event.paths.length || !event.position) {
        return;
      }
      const scale = window.devicePixelRatio || 1;
      const target = document.elementFromPoint(event.position.x / scale, event.position.y / scale);
      const terminalHost = target?.closest<HTMLElement>("[data-terminal-session-id]");
      const sessionId = terminalHost?.dataset.terminalSessionId;
      if (sessionId) {
        pasteTerminalInput(sessionId, event.paths.join(" "));
      }
    })
      .then((unlisten) => {
        if (disposed) {
          unlisten();
          return;
        }
        dispose = unlisten;
      })
      .catch((error) => setErrorMessage(getErrorMessage(error)));

    return () => {
      disposed = true;
      dispose?.();
    };
  }, []);

  useEffect(() => {
    if (!sidebarContextMenu) {
      return;
    }

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setSidebarContextMenu(null);
      }
    };

    const handleWindowBlur = () => {
      setSidebarContextMenu(null);
    };

    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("blur", handleWindowBlur);

    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("blur", handleWindowBlur);
    };
  }, [sidebarContextMenu]);

  useEffect(() => {
    if (!paneMoveMenu) {
      return;
    }

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setPaneMoveMenu(null);
      }
    };

    const handleWindowBlur = () => {
      setPaneMoveMenu(null);
    };

    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("blur", handleWindowBlur);

    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("blur", handleWindowBlur);
    };
  }, [paneMoveMenu]);

  useEffect(() => {
    if (!draggingPaneId) {
      return;
    }
    const pointerId = dragPointerIdRef.current;

    const getCurrentDropTarget = (clientX: number, clientY: number) =>
      getPaneDropTargetFromPoint(clientX, clientY, draggingPaneId);

    const handlePointerMove = (event: PointerEvent) => {
      if (!isPointerMatch(event)) {
        return;
      }
      const nextTarget = getCurrentDropTarget(event.clientX, event.clientY);
      setDragTarget((current) => {
        if (!nextTarget) {
          return null;
        }
        if (current?.paneId === nextTarget.paneId && current.placement === nextTarget.placement) {
          return current;
        }
        return nextTarget;
      });
    };

    const clearPointerDragState = () => {
      const dragHandle = dragHandleElementRef.current;
      const activePointerId = dragPointerIdRef.current;
      if (dragHandle && activePointerId !== null) {
        try {
          if (dragHandle.hasPointerCapture(activePointerId)) {
            dragHandle.releasePointerCapture(activePointerId);
          }
        } catch {
          // no-op: release may fail when capture is already gone.
        }
      }
      dragHandleElementRef.current = null;
      dragPointerIdRef.current = null;
      setDraggingPaneId(null);
      setDragTarget(null);
    };

    const isPointerMatch = (event: PointerEvent) => pointerId === null || event.pointerId === pointerId;

    const handlePointerUp = (event: PointerEvent) => {
      if (!isPointerMatch(event)) {
        return;
      }
      const finalTarget = getCurrentDropTarget(event.clientX, event.clientY);
      const sourcePaneId = draggingPaneIdRef.current;
      clearPointerDragState();
      if (!sourcePaneId || !finalTarget) {
        return;
      }
      void movePaneWithinWindowRef.current(sourcePaneId, finalTarget.paneId, finalTarget.placement).catch(showError);
    };

    const handlePointerCancel = (event: PointerEvent) => {
      if (!isPointerMatch(event)) {
        return;
      }
      clearPointerDragState();
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") {
        return;
      }
      event.preventDefault();
      clearPointerDragState();
    };

    const handleWindowBlur = () => {
      clearPointerDragState();
    };

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    window.addEventListener("pointercancel", handlePointerCancel);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("blur", handleWindowBlur);

    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      window.removeEventListener("pointercancel", handlePointerCancel);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("blur", handleWindowBlur);
    };
  }, [draggingPaneId]);

  async function refreshProjectOnly(projectId: string) {
    const nextProjectSnapshot = await openProject(projectId);
    mergeProjectSnapshot(nextProjectSnapshot);
    const nextProjects = await listProjects();
    setProjects(nextProjects);
    const activeProjectId = activeProjectIdRef.current;
    const activeWorkspaceSessionId = activeWorkspaceSessionIdRef.current;
    if (
      projectId === activeProjectId
      && activeWorkspaceSessionId
      && !nextProjectSnapshot.sessions.some((entry) => entry.id === activeWorkspaceSessionId)
    ) {
      activeWorkspaceSessionIdRef.current = null;
      setActiveWorkspaceSessionId(null);
      setSessionSnapshot(null);
      setSidebarStatus(null);
    }
  }

  async function selectProject(projectId: string) {
    const requestSeq = selectionRequestSeqRef.current + 1;
    selectionRequestSeqRef.current = requestSeq;
    setLoading(true);
    setErrorMessage(null);
    setSidebarContextMenu(null);
    setPaneMoveMenu(null);
    try {
      activeProjectIdRef.current = projectId;
      activeWorkspaceSessionIdRef.current = null;
      setActiveProjectId(projectId);
      setActiveWorkspaceSessionId(null);
      setSessionSnapshot(null);
      setSidebarStatus(null);
      setExpandedProjectIds((current) => ({ ...current, [projectId]: true }));
      const [nextProjectSnapshot, nextProjects] = await Promise.all([
        openProject(projectId),
        listProjects(),
      ]);
      if (!isSelectionRequestCurrent(requestSeq, projectId, null)) {
        return;
      }
      mergeProjectSnapshot(nextProjectSnapshot);
      setProjects(nextProjects);
    } catch (error) {
      if (selectionRequestSeqRef.current === requestSeq) {
        showError(error);
      }
    } finally {
      if (selectionRequestSeqRef.current === requestSeq) {
        setLoading(false);
      }
    }
  }

  async function selectWorkspaceSession(projectId: string, workspaceSessionId: string) {
    const requestSeq = selectionRequestSeqRef.current + 1;
    selectionRequestSeqRef.current = requestSeq;
    setLoading(true);
    setErrorMessage(null);
    setSidebarContextMenu(null);
    setPaneMoveMenu(null);
    try {
      activeProjectIdRef.current = projectId;
      activeWorkspaceSessionIdRef.current = workspaceSessionId;
      setActiveProjectId(projectId);
      setActiveWorkspaceSessionId(workspaceSessionId);
      setExpandedProjectIds((current) => ({ ...current, [projectId]: true }));
      const [nextProjectSnapshot, nextSessionSnapshot, nextProjects] = await Promise.all([
        openProject(projectId),
        openSession(projectId, workspaceSessionId),
        listProjects(),
      ]);
      if (!isSelectionRequestCurrent(requestSeq, projectId, workspaceSessionId)) {
        return;
      }
      mergeProjectSnapshot(nextProjectSnapshot);
      setSessionSnapshot(nextSessionSnapshot);
      setProjects(nextProjects);
    } catch (error) {
      if (selectionRequestSeqRef.current === requestSeq) {
        showError(error);
      }
    } finally {
      if (selectionRequestSeqRef.current === requestSeq) {
        setLoading(false);
      }
    }
  }

  async function refreshSessionSnapshot(promise: Promise<WorkspaceSnapshot>) {
    const mutationSeq = workspaceMutationSeqRef.current + 1;
    workspaceMutationSeqRef.current = mutationSeq;
    const expectedProjectId = activeProjectIdRef.current;
    const expectedWorkspaceSessionId = activeWorkspaceSessionIdRef.current;
    try {
      setErrorMessage(null);
      const nextSnapshot = await promise;
      if (workspaceMutationSeqRef.current !== mutationSeq) {
        return;
      }
      if (
        expectedProjectId !== activeProjectIdRef.current
        || expectedWorkspaceSessionId !== activeWorkspaceSessionIdRef.current
      ) {
        return;
      }
      setSessionSnapshot(nextSnapshot);
      if (expectedProjectId) {
        const nextProjectSnapshot = await openProject(expectedProjectId);
        if (
          workspaceMutationSeqRef.current !== mutationSeq
          || expectedProjectId !== activeProjectIdRef.current
          || expectedWorkspaceSessionId !== activeWorkspaceSessionIdRef.current
        ) {
          return;
        }
        mergeProjectSnapshot(nextProjectSnapshot);
      }
    } catch (error) {
      if (workspaceMutationSeqRef.current === mutationSeq) {
        showError(error);
      }
    }
  }

  async function submitProject() {
    if (!newProjectName.trim() || !newProjectPath.trim()) {
      return;
    }
    try {
      const project = await createProject(newProjectName.trim(), newProjectPath.trim());
      setIsProjectModalOpen(false);
      setNewProjectName("");
      setNewProjectPath("");
      await selectProject(project.id);
    } catch (error) {
      showError(error);
    }
  }

  async function browseForProjectPath() {
    try {
      const selected = await openDirectoryDialog();
      if (selected) {
        setNewProjectPath(selected);
      }
    } catch (error) {
      showError(error);
    }
  }

  async function launchFromPane(paneId: string, profile: LauncherProfile) {
    if (!activeProjectId || !activeWorkspaceSessionId || !activeWindow || launchingPaneIds.includes(paneId)) {
      return;
    }
    setLaunchingPaneIds((current) => [...current, paneId]);
    try {
      await refreshSessionSnapshot(withTimeout(createSession({
        projectId: activeProjectId,
        workspaceSessionId: activeWorkspaceSessionId,
        windowId: activeWindow.id,
        stackId: paneId,
        title: profile.sessionTitle,
        program: profile.program,
        args: profile.args,
        launchProfile: profile.launchProfile,
      }), 12000, "Session start timed out. The launcher was unlocked so you can try again."));
    } finally {
      setLaunchingPaneIds((current) => current.filter((entry) => entry !== paneId));
    }
  }

  function showError(error: unknown) {
    setErrorMessage(getErrorMessage(error));
  }

  function resolveContextMenuPosition(clientX: number, clientY: number) {
    const menuWidth = 196;
    const menuHeight = 172;
    const padding = 10;
    return {
      x: Math.max(padding, Math.min(clientX, window.innerWidth - menuWidth - padding)),
      y: Math.max(padding, Math.min(clientY, window.innerHeight - menuHeight - padding)),
    };
  }

  function openProjectContextMenu(event: ReactMouseEvent<HTMLElement>, project: Project) {
    event.preventDefault();
    setPaneMoveMenu(null);
    const position = resolveContextMenuPosition(event.clientX, event.clientY);
    setSidebarContextMenu({
      kind: "project",
      project,
      x: position.x,
      y: position.y,
    });
  }

  function openSessionContextMenu(event: ReactMouseEvent<HTMLElement>, project: Project, session: WorkspaceSession) {
    event.preventDefault();
    setPaneMoveMenu(null);
    const position = resolveContextMenuPosition(event.clientX, event.clientY);
    setSidebarContextMenu({
      kind: "session",
      project,
      session,
      x: position.x,
      y: position.y,
    });
  }

  async function commitProjectRename(projectId: string) {
    const nextName = renamingProjectName.trim();
    if (!nextName) {
      showError(new Error("Project name is required"));
      setRenamingProjectId(null);
      setRenamingProjectName("");
      return;
    }
    await renameProject(projectId, nextName);
    setRenamingProjectId(null);
    setRenamingProjectName("");
    await refreshProjectOnly(projectId);
  }

  async function createAndOpenSession(projectId: string) {
    const session = await createWorkspaceSession(projectId);
    await selectWorkspaceSession(projectId, session.id);
  }

  async function activatePane(paneId: string) {
    if (!activeProjectId || !activeWorkspaceSessionId || !activeWindow) {
      return;
    }
    if (activeWindow.activePaneId === paneId) {
      return;
    }
    await refreshSessionSnapshot(setActivePane(activeProjectId, activeWorkspaceSessionId, activeWindow.id, paneId));
  }

  async function movePaneWithinWindow(sourcePaneId: string, targetPaneId: string, placement: PaneMovePlacement) {
    if (!activeProjectId || !activeWorkspaceSessionId || !activeWindow) {
      return;
    }
    await refreshSessionSnapshot(
      movePane(activeProjectId, activeWorkspaceSessionId, activeWindow.id, sourcePaneId, targetPaneId, placement),
    );
  }

  movePaneWithinWindowRef.current = movePaneWithinWindow;

  function getPaneDropTargetFromPoint(clientX: number, clientY: number, sourcePaneId: string) {
    const target = document.elementFromPoint(clientX, clientY);
    const paneElement = target instanceof HTMLElement ? target.closest<HTMLElement>("[data-pane-id]") : null;
    const paneId = paneElement?.dataset.paneId;
    if (!paneElement || !paneId || paneId === sourcePaneId) {
      return null;
    }
    return {
      paneId,
      placement: resolveDropPlacementFromRect(paneElement.getBoundingClientRect(), clientX, clientY),
    };
  }

  function beginPaneDrag(event: ReactPointerEvent<HTMLElement>, paneId: string) {
    event.preventDefault();
    event.stopPropagation();
    const previousHandle = dragHandleElementRef.current;
    const previousPointerId = dragPointerIdRef.current;
    if (previousHandle && previousPointerId !== null) {
      try {
        if (previousHandle.hasPointerCapture(previousPointerId)) {
          previousHandle.releasePointerCapture(previousPointerId);
        }
      } catch {
        // ignore stale pointer capture release errors
      }
    }
    dragHandleElementRef.current = event.currentTarget;
    dragPointerIdRef.current = event.pointerId;
    try {
      event.currentTarget.setPointerCapture(event.pointerId);
    } catch {
      // Some platforms may reject capture when pointer state changed.
    }
    setPaneMoveMenu(null);
    setDraggingPaneId(paneId);
    setDragTarget(null);
  }

  function openPaneMoveMenu(event: ReactMouseEvent<HTMLElement>, paneId: string) {
    event.preventDefault();
    event.stopPropagation();
    const position = resolveContextMenuPosition(event.clientX, event.clientY);
    setSidebarContextMenu(null);
    setPaneMoveMenu({
      paneId,
      x: position.x,
      y: position.y,
    });
  }

  function findDirectionalPaneTarget(sourcePaneId: string, placement: DirectionalPaneMovePlacement) {
    const sourceElement = Array.from(document.querySelectorAll<HTMLElement>("[data-pane-id]"))
      .find((element) => element.dataset.paneId === sourcePaneId);
    if (!sourceElement) {
      return null;
    }

    const sourceRect = sourceElement.getBoundingClientRect();
    const sourceCenterX = sourceRect.left + sourceRect.width / 2;
    const sourceCenterY = sourceRect.top + sourceRect.height / 2;
    const paneElements = Array.from(document.querySelectorAll<HTMLElement>("[data-pane-id]"))
      .filter((element) => element.dataset.paneId && element.dataset.paneId !== sourcePaneId);

    let bestMatch: { paneId: string; score: number } | null = null;

    for (const paneElement of paneElements) {
      const paneId = paneElement.dataset.paneId;
      if (!paneId) {
        continue;
      }
      const rect = paneElement.getBoundingClientRect();
      const centerX = rect.left + rect.width / 2;
      const centerY = rect.top + rect.height / 2;
      const deltaX = centerX - sourceCenterX;
      const deltaY = centerY - sourceCenterY;

      let primaryDistance: number | null = null;
      let secondaryDistance = 0;

      if (placement === "left" && deltaX < -4) {
        primaryDistance = Math.abs(deltaX);
        secondaryDistance = Math.abs(deltaY);
      } else if (placement === "right" && deltaX > 4) {
        primaryDistance = Math.abs(deltaX);
        secondaryDistance = Math.abs(deltaY);
      } else if (placement === "top" && deltaY < -4) {
        primaryDistance = Math.abs(deltaY);
        secondaryDistance = Math.abs(deltaX);
      } else if (placement === "bottom" && deltaY > 4) {
        primaryDistance = Math.abs(deltaY);
        secondaryDistance = Math.abs(deltaX);
      }

      if (primaryDistance === null) {
        continue;
      }

      const score = primaryDistance * 1000 + secondaryDistance;
      if (!bestMatch || score < bestMatch.score) {
        bestMatch = { paneId, score };
      }
    }

    return bestMatch?.paneId ?? null;
  }

  async function handlePaneMoveMenuAction(sourcePaneId: string, placement: DirectionalPaneMovePlacement) {
    setPaneMoveMenu(null);
    const targetPaneId = findDirectionalPaneTarget(sourcePaneId, placement);
    if (!targetPaneId) {
      showError(new Error(`No pane available to move ${placement}.`));
      return;
    }
    await movePaneWithinWindow(sourcePaneId, targetPaneId, placement);
  }

  async function handleProjectContextAction(action: "rename" | "delete", project: Project) {
    setSidebarContextMenu(null);
    if (action === "rename") {
      setRenamingProjectId(project.id);
      setRenamingProjectName(project.name);
      return;
    }
    setPendingDeleteProject(project);
  }

  async function handleSessionContextAction(action: "open" | "rename" | "delete", project: Project, session: WorkspaceSession) {
    setSidebarContextMenu(null);
    if (action === "open") {
      await selectWorkspaceSession(project.id, session.id);
      return;
    }
    if (action === "rename") {
      const nextName = window.prompt("Rename session", session.name);
      if (!nextName?.trim()) {
        return;
      }
      try {
        await renameWorkspaceSession(project.id, session.id, nextName.trim());
        await refreshProjectOnly(project.id);
      } catch (error) {
        showError(error);
      }
      return;
    }
    setPendingDeleteSession({ projectId: project.id, session });
  }

  function renderLauncherSurface(onLaunch: (profile: LauncherProfile) => void) {
    return (
      <div className="launcher-surface">
        <div className="launcher-header">
          <div>
            <p className="eyebrow">Launch Surface</p>
            <h3>{activeWorkspaceSession?.name ?? activeProject?.name ?? "Workspace Session"}</h3>
          </div>
        </div>
        <p className="launcher-path" title={activeProject?.path ?? ""}>{activeProject?.path ?? ""}</p>
        <div className="launcher-grid">
          {launcherRows.map((row, rowIndex) => (
            <div key={`launcher-row-${rowIndex + 1}`} className={`launcher-row launcher-row-${rowIndex + 1}`}>
              {row.map((profile) => (
                <button key={profile.id} className={`launcher-tile ${profile.theme}`} onClick={() => void onLaunch(profile)}>
                  <span className="launcher-tile-title">{profile.title}</span>
                  <span className="launcher-tile-desc">{profile.description}</span>
                </button>
              ))}
            </div>
          ))}
        </div>
      </div>
    );
  }

  function renderNode(node: LayoutNode, canClosePane = false): ReactNode {
    if (isSplitNode(node)) {
      const splitLayout = getResolvedSplitLayout(node, panelLayouts[node.id]);
      const splitKey = `${node.id}:${node.children.map((child) => child.id).join(":")}`;
      return (
        <PanelGroup
          key={splitKey}
          id={node.id}
          orientation={node.direction === "horizontal" ? "horizontal" : "vertical"}
          defaultLayout={splitLayout}
          onLayoutChanged={(sizes) => setPanelLayouts((current) => ({ ...current, [node.id]: sizes }))}
          className="panel-group"
        >
          {node.children.map((child) => (
            <Panel key={child.id} id={child.id} minSize={getMinimumPanelSize(node.direction)} defaultSize={splitLayout[child.id] ?? undefined}>
              {renderNode(child, true)}
            </Panel>
          )).flatMap((panel, index, array) => index < array.length - 1 ? [panel, <PanelResizeHandle key={`resize-${index}`} className="resize-handle" />] : [panel])}
        </PanelGroup>
      );
    }

    const activeItem = node.items.find((item) => item.id === node.activeItemId) ?? node.items[0];
    const session = activeItem?.sessionId ? sessions.get(activeItem.sessionId) : undefined;
    const sourcePaneLabel = node.sourcePaneId ? paneLabels.get(node.sourcePaneId) : null;
    const originLabel = node.createdBy === "ai" ? (sourcePaneLabel ? `AI from ${sourcePaneLabel}` : "AI") : null;
    const isActivePane = activeWindow?.activePaneId === node.id;
    const showDropOverlay = Boolean(draggingPaneId && draggingPaneId !== node.id);
    const dropPlacement = dragTarget?.paneId === node.id ? dragTarget.placement : null;

    return (
      <div
        data-pane-id={node.id}
        className={`stack-pane ${node.createdBy === "ai" ? "ai-pane" : "user-pane"} ${isActivePane ? "active" : ""} ${draggingPaneId === node.id ? "drag-source" : ""}`}
        onMouseDown={() => {
          setPaneMoveMenu(null);
          void activatePane(node.id).catch(showError);
        }}
      >
        <div className="stack-toolbar">
          <div className="stack-primary">
            <button
              className="pane-drag-handle"
              title="Drag to move pane"
              aria-label={`Move ${node.paneLabel}`}
              onMouseDown={(event) => event.stopPropagation()}
              onPointerDown={(event) => beginPaneDrag(event, node.id)}
              onClick={(event) => event.stopPropagation()}
            >
              ::
            </button>
            <span className={`pane-badge ${node.createdBy === "ai" ? "ai" : "user"}`}>{node.paneLabel}</span>
            {originLabel ? <span className="pane-origin">{originLabel}</span> : null}
            <div className="pane-label" title={activeItem?.title ?? "Pane"}>{activeItem?.title ?? "Pane"}</div>
          </div>
          <div className="stack-actions">
            {session ? <span className={`status-pill toolbar-status ${session.status}`}>{session.status}</span> : null}
            <button className="toolbar-button" title="Split Up/Down" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(splitPane(activeProjectId, activeWorkspaceSessionId, activeWindow.id, node.id, "vertical"))}>━</button>
            <button className="toolbar-button" title="Split Left/Right" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(splitPane(activeProjectId, activeWorkspaceSessionId, activeWindow.id, node.id, "horizontal"))}>┃</button>
            <button
              className="toolbar-button"
              title="Move Pane"
              onMouseDown={(event) => event.stopPropagation()}
              onClick={(event) => openPaneMoveMenu(event, node.id)}
            >
              Move
            </button>
            {canClosePane ? <button className="toolbar-button subtle" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(closePane(activeProjectId, activeWorkspaceSessionId, activeWindow.id, node.id))}>Close Pane</button> : null}
          </div>
        </div>
        <div className="stack-body">
          {showDropOverlay ? (
            <div className="pane-drop-overlay" aria-hidden>
              <span className={`pane-drop-zone top ${dropPlacement === "top" ? "active" : ""}`} />
              <span className={`pane-drop-zone left ${dropPlacement === "left" ? "active" : ""}`} />
              <span className={`pane-drop-zone center ${dropPlacement === "swap" ? "active" : ""}`} />
              <span className={`pane-drop-zone right ${dropPlacement === "right" ? "active" : ""}`} />
              <span className={`pane-drop-zone bottom ${dropPlacement === "bottom" ? "active" : ""}`} />
            </div>
          ) : null}
          {session && session.status === "running" ? (
            <TerminalView
              sessionId={session.id}
              variant={node.createdBy === "ai" ? "ai" : "user"}
              onActivate={() => void activatePane(node.id).catch(showError)}
            />
          ) : node.createdBy === "user" && node.launchState === "unlaunched" ? (
            renderLauncherSurface((profile) => launchFromPane(node.id, profile))
          ) : session ? (
            <div className="empty-stack">
              <p>{`${node.paneLabel} · ${session.title}`}</p>
              <p>Session is {session.status}. Start a new terminal in this pane.</p>
              <button className="toolbar-button primary" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(createSession({
                projectId: activeProjectId,
                workspaceSessionId: activeWorkspaceSessionId,
                windowId: activeWindow.id,
                stackId: node.id,
                title: session.title,
                program: session.program,
                args: session.args ?? undefined,
                launchProfile: session.launchProfile,
              }))}>Start Replacement Session</button>
            </div>
          ) : (
            <div className="empty-stack">
              <p>{`${node.paneLabel} · No session attached`}</p>
              <button className="toolbar-button primary" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(createSession({
                projectId: activeProjectId,
                workspaceSessionId: activeWorkspaceSessionId,
                windowId: activeWindow.id,
                stackId: node.id,
                launchProfile: "terminal",
              }))}>Start Terminal</button>
            </div>
          )}
        </div>
      </div>
    );
  }

  return (
    <AppErrorBoundary>
      <div className="app-shell">
        <aside className="sidebar">
          <div className="sidebar-header">
            <div className="sidebar-brand">
              <img className="sidebar-logo" src={vantaraLogo} alt="Vantara" />
            </div>
            <div className="sidebar-section-header">
              <h1>Projects</h1>
              <button className="toolbar-button primary" onClick={() => setIsProjectModalOpen(true)}>Add Project</button>
            </div>
          </div>
          {errorMessage ? <div className="error-banner">{errorMessage}</div> : null}
          <div className="sidebar-tree">
            {projects.map((project) => {
              const projectSessions = projectSnapshotsById[project.id]?.sessions ?? [];
              return (
                <div key={project.id} className={`tree-group ${project.id === activeProjectId ? "active" : ""}`}>
                  {renamingProjectId === project.id ? (
                    <div className="tree-row tree-row-project tree-row-static">
                      <span className="project-color" style={{ background: project.color }} />
                      <div className="tree-inline-edit">
                        <input
                          className="project-inline-input"
                          value={renamingProjectName}
                          onChange={(event) => setRenamingProjectName(event.target.value)}
                          onBlur={() => void commitProjectRename(project.id).catch(showError)}
                          onKeyDown={(event) => {
                            if (event.key === "Enter") {
                              void commitProjectRename(project.id).catch(showError);
                            } else if (event.key === "Escape") {
                              setRenamingProjectId(null);
                              setRenamingProjectName("");
                            }
                          }}
                          autoFocus
                        />
                      </div>
                    </div>
                  ) : (
                    <>
                      <div
                        className={`tree-row tree-row-project ${project.id === activeProjectId ? "active" : ""}`}
                        onClick={() => void selectProject(project.id)}
                        onContextMenu={(event) => openProjectContextMenu(event, project)}
                      >
                        <span className="project-color" style={{ background: project.color }} />
                        <button
                          className={`tree-disclosure ${expandedProjectIds[project.id] ? "expanded" : ""}`}
                          aria-label={expandedProjectIds[project.id] ? "Collapse sessions" : "Expand sessions"}
                          onClick={(event) => {
                            event.stopPropagation();
                            setExpandedProjectIds((current) => ({ ...current, [project.id]: !current[project.id] }));
                          }}
                        >
                          <span className="tree-disclosure-glyph">{">"}</span>
                        </button>
                        <div className="tree-row-body">
                          <strong>{project.name}</strong>
                          <div className="project-path">{project.path}</div>
                        </div>
                        <span className="tree-row-surface">Project</span>
                      </div>
                      {expandedProjectIds[project.id] ? (
                        <div className="tree-children">
                          {projectSessions.map((session) => (
                            <div
                              key={session.id}
                              className={`tree-row tree-row-session ${session.id === activeWorkspaceSessionId ? "active" : ""}`}
                              onClick={() => void selectWorkspaceSession(project.id, session.id)}
                              onContextMenu={(event) => openSessionContextMenu(event, project, session)}
                            >
                              <span className="tree-branch" />
                              <span className={`tree-session-badge ${session.createdBy === "ai" ? "ai" : "user"}`}>
                                {session.createdBy === "ai" ? "AI" : "S"}
                              </span>
                              <div className="tree-row-body">
                                <strong>{session.name}</strong>
                                <div className="project-path">{session.createdBy === "ai" ? "AI session" : "Session"}</div>
                              </div>
                              <span className="tree-row-surface">Session</span>
                            </div>
                          ))}
                          <div className="tree-inline-action-row">
                            <span className="tree-branch" />
                            <button
                              className="tree-inline-action-button"
                              onClick={() => void createAndOpenSession(project.id).catch(showError)}
                            >
                              New Session
                            </button>
                          </div>
                        </div>
                      ) : null}
                    </>
                  )}
                </div>
              );
            })}
          </div>
          <div className="sidebar-status-panel">
            <div className="sidebar-status-header">
              <p className="eyebrow">Active Pane</p>
              <span className={`status-pill ${sidebarStatus?.state ?? "exited"}`}>
                {activeTerminalSession ? (sidebarStatus?.state ?? activeTerminalSession.status) : activeWorkspaceSession ? "idle" : "none"}
              </span>
            </div>
            {activeTerminalSession && sidebarStatus ? (
              <>
                <div className="sidebar-status-primary">
                  <span className={`sidebar-provider-badge ${sidebarStatus.provider}`}>
                    {sidebarStatus.provider === "claude" ? "Claude" : sidebarStatus.provider === "codex" ? "Codex" : "Terminal"}
                  </span>
                  {sidebarStatus.modelLabel ? <strong>{sidebarStatus.modelLabel}</strong> : <strong>{activeTerminalSession.title}</strong>}
                </div>
                <div className="sidebar-status-chips">
                  {sidebarStatus.modeLabel ? <span className="sidebar-status-chip">{sidebarStatus.modeLabel}</span> : null}
                  {sidebarStatus.contextPercent !== null && sidebarStatus.contextPercent !== undefined ? <span className="sidebar-status-chip">{`Context ${sidebarStatus.contextPercent}%`}</span> : null}
                  {sidebarStatus.usage5hPercent !== null && sidebarStatus.usage5hPercent !== undefined ? <span className="sidebar-status-chip">{`5h ${sidebarStatus.usage5hPercent}%`}</span> : null}
                  {sidebarStatus.usage7dPercent !== null && sidebarStatus.usage7dPercent !== undefined ? <span className="sidebar-status-chip">{`7d ${sidebarStatus.usage7dPercent}%`}</span> : null}
                </div>
                <div className="sidebar-status-foot">
                  <span>{activeWorkspaceSession?.name ?? "Session"}</span>
                  <span>{activeWindow?.title ?? "Window"}</span>
                </div>
              </>
            ) : activeWorkspaceSession ? (
              <div className="sidebar-status-empty">
                <strong>Idle</strong>
                <span>Select or launch a terminal in the active pane.</span>
              </div>
            ) : (
              <div className="sidebar-status-empty">
                <strong>No active session</strong>
                <span>Open a session to see provider, model, and runtime status.</span>
              </div>
            )}
          </div>
        </aside>

        <main className="workspace">
          <header className="workspace-header">
            <div>
              <p className="eyebrow">{activeWorkspaceSession ? "Session Workspace" : "Project Sessions"}</p>
              <h2>{activeWorkspaceSession?.name ?? activeProject?.name ?? "No project selected"}</h2>
            </div>
            {activeProjectId && activeWorkspaceSession ? (
              <button className="toolbar-button primary" onClick={() => void refreshSessionSnapshot(createWindow(activeProjectId, activeWorkspaceSession.id))}>New Window</button>
            ) : null}
          </header>

          {activeWorkspaceSession && sessionSnapshot ? (
            <>
              <div className="workspace-tabs">
                {sessionSnapshot.windows.map((windowTab) => (
                  <button key={windowTab.id} className={`workspace-tab ${sessionSnapshot.activeWindowId === windowTab.id ? "active" : ""}`} onClick={() => activeProjectId && void refreshSessionSnapshot(setActiveWindow(activeProjectId, activeWorkspaceSession.id, windowTab.id))} onDoubleClick={() => { const nextTitle = window.prompt("Rename window", windowTab.title); if (nextTitle?.trim() && activeProjectId) { void refreshSessionSnapshot(renameWindow(activeProjectId, activeWorkspaceSession.id, windowTab.id, nextTitle.trim())); } }}>
                    <span>{windowTab.title}</span>
                    <span className="workspace-tab-close" onClick={(event) => { event.stopPropagation(); if (activeProjectId) { void refreshSessionSnapshot(closeWindow(activeProjectId, activeWorkspaceSession.id, windowTab.id)); } }}>x</span>
                  </button>
                ))}
              </div>
              <section className="workspace-canvas" ref={workspaceCanvasRef}>
                {loading ? <div className="empty-state">Loading workspace...</div> : activeWindow ? renderNode(activeWindow.root, false) : <div className="empty-state">Preparing session window...</div>}
              </section>
            </>
          ) : (
            <section className="workspace-canvas">
              {loading ? <div className="empty-state">Loading project...</div> : activeProject ? (
                <div className="project-list">
                  {(activeProjectSnapshot?.sessions ?? []).map((session) => (
                    <div key={session.id} className="session-list-card">
                      <button className="session-list-card-main" onClick={() => void selectWorkspaceSession(activeProject.id, session.id)}>
                        <div className="session-list-card-content">
                          <strong>{session.name}</strong>
                          <div className="session-list-card-meta">
                            <span className={`tree-session-badge ${session.createdBy === "ai" ? "ai" : "user"}`}>
                              {session.createdBy === "ai" ? "AI" : "S"}
                            </span>
                            <span>{session.createdBy === "ai" ? "AI generated session" : "User session"}</span>
                          </div>
                        </div>
                      </button>
                    </div>
                  ))}
                </div>
              ) : <div className="empty-state">Create a project to begin.</div>}
            </section>
          )}
        </main>

        {isProjectModalOpen ? <div className="modal-backdrop" onClick={(event) => event.target === event.currentTarget && setIsProjectModalOpen(false)}><div className="modal-card"><div className="modal-header"><div><p className="eyebrow">New Project</p><h3>Add a local workspace</h3></div><button className="toolbar-button" onClick={() => setIsProjectModalOpen(false)}>Close</button></div><div className="project-form modal-form"><input placeholder="Project name" value={newProjectName} onChange={(event) => setNewProjectName(event.target.value)} autoFocus /><div className="path-field"><input placeholder="Absolute path" value={newProjectPath} onChange={(event) => setNewProjectPath(event.target.value)} /><button className="toolbar-button" onClick={() => void browseForProjectPath()}>Browse</button></div><div className="modal-actions"><button className="toolbar-button" onClick={() => setIsProjectModalOpen(false)}>Cancel</button><button className="toolbar-button primary" onClick={() => void submitProject()}>Add Project</button></div></div></div></div> : null}

        {pendingDeleteProject ? <div className="modal-backdrop" onClick={(event) => event.target === event.currentTarget && setPendingDeleteProject(null)}><div className="modal-card modal-card-compact"><div className="modal-header"><div><p className="eyebrow">Remove Project</p><h3>{pendingDeleteProject.name}</h3></div><button className="toolbar-button" onClick={() => setPendingDeleteProject(null)}>Close</button></div><p className="modal-copy">This removes the project from Vantara only. The actual folder stays on disk.</p><div className="modal-actions"><button className="toolbar-button" onClick={() => setPendingDeleteProject(null)}>Cancel</button><button className="toolbar-button subtle" onClick={() => { const projectToDelete = pendingDeleteProject; void deleteProject(projectToDelete.id).then(async (result) => { setPendingDeleteProject(null); const nextProjects = await listProjects(); setProjects(nextProjects); setProjectSnapshotsById((current) => { const next = { ...current }; delete next[projectToDelete.id]; return next; }); if (activeProjectId === projectToDelete.id) { if (result.nextProjectId) { await selectProject(result.nextProjectId); } else { activeProjectIdRef.current = null; activeWorkspaceSessionIdRef.current = null; activeTerminalSessionIdRef.current = null; setActiveProjectId(null); setActiveWorkspaceSessionId(null); setSessionSnapshot(null); setSidebarStatus(null); } } }).catch(showError); }}>Delete Project</button></div></div></div> : null}

        {pendingDeleteSession ? <div className="modal-backdrop" onClick={(event) => event.target === event.currentTarget && setPendingDeleteSession(null)}><div className="modal-card modal-card-compact"><div className="modal-header"><div><p className="eyebrow">Remove Session</p><h3>{pendingDeleteSession.session.name}</h3></div><button className="toolbar-button" onClick={() => setPendingDeleteSession(null)}>Close</button></div><p className="modal-copy">This removes the session node and its runtime terminals. The project remains intact.</p><div className="modal-actions"><button className="toolbar-button" onClick={() => setPendingDeleteSession(null)}>Cancel</button><button className="toolbar-button subtle" onClick={() => { const sessionToDelete = pendingDeleteSession; void deleteWorkspaceSession(sessionToDelete.projectId, sessionToDelete.session.id).then((nextSnapshot) => { setPendingDeleteSession(null); mergeProjectSnapshot(nextSnapshot); if (activeProjectId === sessionToDelete.projectId && activeWorkspaceSessionId === sessionToDelete.session.id) { activeWorkspaceSessionIdRef.current = null; activeTerminalSessionIdRef.current = null; setActiveWorkspaceSessionId(null); setSessionSnapshot(null); setSidebarStatus(null); } }).catch(showError); }}>Delete Session</button></div></div></div> : null}

        {paneMoveMenu ? (
          <div className="context-menu-layer" onClick={() => setPaneMoveMenu(null)} onContextMenu={(event) => event.preventDefault()}>
            <div
              className="context-menu"
              style={{ left: paneMoveMenu.x, top: paneMoveMenu.y }}
              onClick={(event) => event.stopPropagation()}
              onContextMenu={(event) => event.preventDefault()}
            >
              <button className="context-menu-item" onClick={() => void handlePaneMoveMenuAction(paneMoveMenu.paneId, "left").catch(showError)}>Move Left</button>
              <button className="context-menu-item" onClick={() => void handlePaneMoveMenuAction(paneMoveMenu.paneId, "right").catch(showError)}>Move Right</button>
              <button className="context-menu-item" onClick={() => void handlePaneMoveMenuAction(paneMoveMenu.paneId, "top").catch(showError)}>Move Up</button>
              <button className="context-menu-item" onClick={() => void handlePaneMoveMenuAction(paneMoveMenu.paneId, "bottom").catch(showError)}>Move Down</button>
            </div>
          </div>
        ) : null}

        {sidebarContextMenu ? (
          <div className="context-menu-layer" onClick={() => setSidebarContextMenu(null)} onContextMenu={(event) => event.preventDefault()}>
            <div
              className="context-menu"
              style={{ left: sidebarContextMenu.x, top: sidebarContextMenu.y }}
              onClick={(event) => event.stopPropagation()}
              onContextMenu={(event) => event.preventDefault()}
            >
              {sidebarContextMenu.kind === "project" ? (
                <>
                  <button className="context-menu-item" onClick={() => void handleProjectContextAction("rename", sidebarContextMenu.project)}>Rename Project</button>
                  <button className="context-menu-item danger" onClick={() => void handleProjectContextAction("delete", sidebarContextMenu.project)}>Delete Project</button>
                </>
              ) : (
                <>
                  <button className="context-menu-item" onClick={() => void handleSessionContextAction("open", sidebarContextMenu.project, sidebarContextMenu.session)}>Open Session</button>
                  <button className="context-menu-item" onClick={() => void handleSessionContextAction("rename", sidebarContextMenu.project, sidebarContextMenu.session)}>Rename Session</button>
                  <button className="context-menu-item danger" onClick={() => void handleSessionContextAction("delete", sidebarContextMenu.project, sidebarContextMenu.session)}>Delete Session</button>
                </>
              )}
            </div>
          </div>
        ) : null}
      </div>
    </AppErrorBoundary>
  );
}
