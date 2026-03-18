import type { ReactNode } from "react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Group as PanelGroup, Panel, Separator as PanelResizeHandle } from "react-resizable-panels";
import type {
  LayoutNode,
  LaunchProfile,
  Project,
  StackItem,
  TerminalSession,
  WorkspaceSnapshot,
} from "@workspace-terminal/contracts";

import { TerminalView } from "./components/TerminalView";
import { AppErrorBoundary } from "./components/AppErrorBoundary";
import {
  closePane,
  createProject,
  createSession,
  createTab,
  closeTab,
  deleteProject,
  listProjects,
  openWorkspace,
  reportTabViewport,
  renameProject,
  renameTab,
  setActiveStackItem,
  setActiveTab,
  splitPane,
} from "./lib/api";
import {
  listenSessionExit,
  listenSessionOutput,
  listenWindowFileDrop,
  listenWorkspaceChanged,
  openDirectoryDialog,
} from "./lib/desktop";
import { getActiveTab, isSplitNode, isStackNode } from "./lib/layout";
import { pasteTerminalInput, writeTerminalChunk } from "./lib/terminalRegistry";

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

const launcherRows: LauncherProfile[][] = [
  [
    {
      id: "claude",
      title: "Claude Code",
      description: "Interactive coding session",
      program: "claude",
      launchProfile: "claude",
      theme: "claude",
      sessionTitle: "Claude Code",
    },
    {
      id: "claude-unsafe",
      title: "Claude Unsafe",
      description: "Bypass permissions",
      program: "claude",
      args: ["--dangerously-skip-permissions"],
      launchProfile: "claudeUnsafe",
      theme: "claude-danger",
      sessionTitle: "Claude Unsafe",
    },
  ],
  [
    {
      id: "codex",
      title: "Codex",
      description: "Interactive agent session",
      program: "codex",
      launchProfile: "codex",
      theme: "codex",
      sessionTitle: "Codex",
    },
    {
      id: "codex-full-auto",
      title: "Codex Full Auto",
      description: "Workspace-write auto mode",
      program: "codex",
      args: ["--full-auto"],
      launchProfile: "codexFullAuto",
      theme: "codex-auto",
      sessionTitle: "Codex Full Auto",
    },
  ],
  [
    {
      id: "terminal",
      title: "Terminal",
      description: "Plain PowerShell shell",
      program: "powershell",
      launchProfile: "terminal",
      theme: "terminal",
      sessionTitle: "PowerShell",
    },
  ],
];

function createSessionMap(snapshot: WorkspaceSnapshot | null) {
  return new Map<string, TerminalSession>(snapshot?.sessions.map((session: TerminalSession) => [session.id, session]) ?? []);
}

function getMinimumPanelSize(direction: "horizontal" | "vertical") {
  return direction === "vertical" ? 24 : 20;
}

function createDefaultSplitLayout(node: Extract<LayoutNode, { type: "split" }>) {
  return Object.fromEntries(
    node.children.map((child: LayoutNode, childIndex: number) => [child.id, node.sizes[childIndex] ?? 50]),
  );
}

function getResolvedSplitLayout(
  node: Extract<LayoutNode, { type: "split" }>,
  savedLayout?: Record<string, number>,
) {
  const fallbackLayout = createDefaultSplitLayout(node);
  if (!savedLayout) {
    return fallbackLayout;
  }

  const childIds = node.children.map((child) => child.id);
  const hasAllChildren = childIds.every((id) => typeof savedLayout[id] === "number");
  const sameChildCount = Object.keys(savedLayout).length === childIds.length;

  if (!hasAllChildren || !sameChildCount) {
    return fallbackLayout;
  }

  return Object.fromEntries(childIds.map((id) => [id, savedLayout[id]]));
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

function withTimeout<T>(promise: Promise<T>, timeoutMs: number, timeoutMessage: string) {
  let timeoutId: number | null = null;
  const timeoutPromise = new Promise<T>((_resolve, reject) => {
    timeoutId = window.setTimeout(() => {
      reject(new Error(timeoutMessage));
    }, timeoutMs);
  });

  return Promise.race([promise, timeoutPromise]).finally(() => {
    if (timeoutId !== null) {
      window.clearTimeout(timeoutId);
    }
  });
}

export default function App() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [activeProjectId, setActiveProjectId] = useState<string | null>(null);
  const [snapshot, setSnapshot] = useState<WorkspaceSnapshot | null>(null);
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
  const workspaceCanvasRef = useRef<HTMLElement | null>(null);

  const sessions = useMemo(() => createSessionMap(snapshot), [snapshot]);
  const activeProject = useMemo(
    () => projects.find((project) => project.id === activeProjectId) ?? null,
    [projects, activeProjectId],
  );
  const activeTab = useMemo(() => getActiveTab(snapshot?.tabs ?? [], snapshot?.activeTabId), [snapshot]);
  const paneLabels = useMemo(() => {
    const nextPaneLabels = new Map<string, string>();
    if (activeTab) {
      collectPaneLabels(activeTab.root, nextPaneLabels);
    }
    return nextPaneLabels;
  }, [activeTab]);

  useEffect(() => {
    void (async () => {
      try {
        const loadedProjects = await listProjects();
        setProjects(loadedProjects);
        if (loadedProjects[0]) {
          setLoading(true);
          try {
            const nextSnapshot = await openWorkspace(loadedProjects[0].id);
            setActiveProjectId(loadedProjects[0].id);
            setSnapshot(nextSnapshot);
          } finally {
            setLoading(false);
          }
        }
      } catch (error) {
        showError(error);
      }
    })();
  }, []);

  useEffect(() => {
    const disposers: Array<() => void> = [];

    void listenSessionOutput((event) => {
      writeTerminalChunk(event.payload.sessionId, event.payload.chunk);
    }).then((dispose) => disposers.push(dispose));

    void listenSessionExit(() => {
      if (activeProjectId) {
        void (async () => {
          setLoading(true);
          try {
            const nextSnapshot = await openWorkspace(activeProjectId);
            setSnapshot(nextSnapshot);
            const reloadedProjects = await listProjects();
            setProjects(reloadedProjects);
          } catch (error) {
            showError(error);
          } finally {
            setLoading(false);
          }
        })();
      }
    }).then((dispose) => disposers.push(dispose));

    void listenWorkspaceChanged((event) => {
      if (!activeProjectId || event.payload.projectId !== activeProjectId) {
        return;
      }

      void (async () => {
        try {
          const nextSnapshot = await openWorkspace(activeProjectId);
          setSnapshot(nextSnapshot);
        } catch (error) {
          showError(error);
        }
      })();
    }).then((dispose) => disposers.push(dispose));

    return () => {
      for (const dispose of disposers) {
        dispose();
      }
    };
  }, [activeProjectId]);

  useEffect(() => {
    const element = workspaceCanvasRef.current;
    if (!element || !activeProjectId || !activeTab) {
      return;
    }

    let frame: number | null = null;
    let lastWidth = 0;
    let lastHeight = 0;

    const sendViewport = () => {
      frame = null;
      const rect = element.getBoundingClientRect();
      const width = Math.round(rect.width);
      const height = Math.round(rect.height);
      if (width <= 0 || height <= 0) {
        return;
      }
      if (width === lastWidth && height === lastHeight) {
        return;
      }
      lastWidth = width;
      lastHeight = height;
      void reportTabViewport(activeProjectId, activeTab.id, width, height).catch((error) => {
        const message = error instanceof Error ? error.message : String(error);
        setErrorMessage(message);
      });
    };

    const scheduleViewportReport = () => {
      if (frame !== null) {
        cancelAnimationFrame(frame);
      }
      frame = requestAnimationFrame(sendViewport);
    };

    const observer = new ResizeObserver(() => {
      scheduleViewportReport();
    });
    observer.observe(element);
    scheduleViewportReport();

    return () => {
      observer.disconnect();
      if (frame !== null) {
        cancelAnimationFrame(frame);
      }
    };
  }, [activeProjectId, activeTab]);

  useEffect(() => {
    let dispose: (() => void) | null = null;

    void listenWindowFileDrop((event) => {
      if (event.type !== "drop" || !event.paths.length || !event.position) {
        return;
      }

      const scale = window.devicePixelRatio || 1;
      const target = document.elementFromPoint(event.position.x / scale, event.position.y / scale);
      const terminalHost = target?.closest<HTMLElement>("[data-terminal-session-id]");
      const sessionId = terminalHost?.dataset.terminalSessionId;
      if (!sessionId) {
        return;
      }

      pasteTerminalInput(sessionId, event.paths.join(" "));
    }).then((unlisten) => {
      dispose = unlisten;
    });

    return () => {
      dispose?.();
    };
  }, []);

  async function selectProject(projectId: string) {
    await loadWorkspace(projectId);
  }

  async function loadWorkspace(projectId: string) {
    setLoading(true);
    setErrorMessage(null);
    try {
      const nextSnapshot = await openWorkspace(projectId);
      setActiveProjectId(projectId);
      setSnapshot(nextSnapshot);
      const reloadedProjects = await listProjects();
      setProjects(reloadedProjects);
    } catch (error) {
      showError(error);
    } finally {
      setLoading(false);
    }
  }

  async function refreshWorkspace(promise: Promise<WorkspaceSnapshot>) {
    try {
      setErrorMessage(null);
      const nextSnapshot = await promise;
      setSnapshot(nextSnapshot);
    } catch (error) {
      showError(error);
    }
  }

  async function submitProject() {
    if (!newProjectName.trim() || !newProjectPath.trim()) {
      return;
    }
    try {
      setErrorMessage(null);
      const project = await createProject(newProjectName.trim(), newProjectPath.trim());
      setNewProjectName("");
      setNewProjectPath("");
      setIsProjectModalOpen(false);
      const reloadedProjects = await listProjects();
      setProjects(reloadedProjects);
      await selectProject(project.id);
    } catch (error) {
      showError(error);
    }
  }

  async function handleRenameTab(tabId: string, currentTitle: string) {
    if (!activeProjectId) {
      return;
    }
    const nextTitle = window.prompt("Rename tab", currentTitle);
    if (!nextTitle?.trim()) {
      return;
    }
    await refreshWorkspace(renameTab(activeProjectId, tabId, nextTitle.trim()));
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

  function showError(error: unknown) {
    const message = error instanceof Error ? error.message : String(error);
    setErrorMessage(message);
  }

  function openProjectModal() {
    setErrorMessage(null);
    setIsProjectModalOpen(true);
  }

  function closeProjectModal() {
    setIsProjectModalOpen(false);
    setNewProjectName("");
    setNewProjectPath("");
  }

  async function launchFromPane(
    paneId: string,
    profile: LauncherProfile,
  ) {
    if (!activeProjectId || !activeTab || launchingPaneIds.includes(paneId)) {
      return;
    }

    setLaunchingPaneIds((current) => [...current, paneId]);
    try {
      console.debug("[launcher] click", {
        paneId,
        profileId: profile.id,
        program: profile.program,
        args: profile.args ?? [],
      });
      await refreshWorkspace(
        withTimeout(
          createSession({
            projectId: activeProjectId,
            tabId: activeTab.id,
            stackId: paneId,
            title: profile.sessionTitle,
            program: profile.program,
            args: profile.args,
            launchProfile: profile.launchProfile,
          }),
          12000,
          "Session start timed out. The launcher was unlocked so you can try again.",
        ),
      );
      setErrorMessage(null);
    } catch (error) {
      showError(error);
    } finally {
      setLaunchingPaneIds((current) => current.filter((entry) => entry !== paneId));
    }
  }

  function beginRenameProject(project: Project) {
    setErrorMessage(null);
    setRenamingProjectId(project.id);
    setRenamingProjectName(project.name);
  }

  function cancelRenameProject() {
    setRenamingProjectId(null);
    setRenamingProjectName("");
  }

  async function commitRenameProject(projectId: string) {
    if (renamingProjectId !== projectId) {
      return;
    }

    const nextName = renamingProjectName.trim();
    if (!nextName) {
      setErrorMessage("Project name cannot be empty.");
      cancelRenameProject();
      return;
    }

    try {
      setErrorMessage(null);
      const updatedProject = await renameProject(projectId, nextName);
      setProjects((current) => current.map((project) => (project.id === updatedProject.id ? updatedProject : project)));
      cancelRenameProject();
    } catch (error) {
      showError(error);
      cancelRenameProject();
    }
  }

  async function confirmDeleteProject() {
    if (!pendingDeleteProject) {
      return;
    }

    try {
      setErrorMessage(null);
      cancelRenameProject();
      const result = await deleteProject(pendingDeleteProject.id);
      setPendingDeleteProject(null);
      setProjects((current) => current.filter((project) => project.id !== result.deletedProjectId));

      if (result.nextProjectId) {
        await loadWorkspace(result.nextProjectId);
        return;
      }

      setActiveProjectId(null);
      setSnapshot(null);
    } catch (error) {
      showError(error);
    }
  }

  function renderLauncher(node: Extract<LayoutNode, { type: "stack" }>) {
    const isLaunching = launchingPaneIds.includes(node.id);
    return (
      <div className="launcher-surface">
        <div className="launcher-header">
          <div>
            <p className="eyebrow">Launch Surface</p>
            <h3>{activeProject?.name ?? "Workspace Project"}</h3>
          </div>
          <div className="launcher-pane-chip">{node.paneLabel}</div>
        </div>
        <p className="launcher-path" title={activeProject?.path ?? ""}>
          {activeProject?.path ?? "Select a workspace to begin"}
        </p>
        <div className="launcher-grid">
          {launcherRows.map((row, rowIndex) => (
            <div
              key={`launcher-row-${rowIndex + 1}`}
              className={`launcher-row launcher-row-${rowIndex + 1}`}
            >
              {row.map((profile) => (
                <button
                  key={profile.id}
                  className={`launcher-tile ${profile.theme}`}
                  disabled={isLaunching}
                  onClick={() => {
                    void launchFromPane(node.id, profile);
                  }}
                >
                  <span className="launcher-tile-title">{profile.title}</span>
                  <span className="launcher-tile-desc">
                    {isLaunching ? "Starting session..." : profile.description}
                  </span>
                </button>
              ))}
            </div>
          ))}
        </div>
      </div>
    );
  }

  function renderNode(node: LayoutNode, canClosePane = false): React.ReactNode {
    if (isSplitNode(node)) {
      const splitLayout = getResolvedSplitLayout(node, panelLayouts[node.id]);
      const splitKey = `${node.id}:${node.children.map((child) => child.id).join(":")}`;

      return (
        <PanelGroup
          key={splitKey}
          id={node.id}
          orientation={node.direction === "horizontal" ? "horizontal" : "vertical"}
          defaultLayout={splitLayout}
          onLayoutChanged={(sizes) => {
            setPanelLayouts((current) => ({
              ...current,
              [node.id]: sizes,
            }));
          }}
          className="panel-group"
        >
          {node.children.map((child: LayoutNode) => (
            <Panel
              key={child.id}
              id={child.id}
              minSize={getMinimumPanelSize(node.direction)}
              defaultSize={splitLayout[child.id] ?? undefined}
            >
              {renderNode(child, true)}
            </Panel>
          )).flatMap((panel: ReactNode, index: number, array: ReactNode[]) =>
            index < array.length - 1
              ? [panel, <PanelResizeHandle key={`resize-${index}`} className="resize-handle" />]
              : [panel],
          )}
        </PanelGroup>
      );
    }

    if (!isStackNode(node)) {
      return null;
    }

    const activeItem = node.items.find((item: StackItem) => item.id === node.activeItemId) ?? node.items[0];
    const session = activeItem?.sessionId ? sessions.get(activeItem.sessionId) : undefined;
    const showStackTabs = node.items.length > 1;
    const sourcePaneLabel = node.sourcePaneId ? paneLabels.get(node.sourcePaneId) : null;
    const originLabel = node.createdBy === "ai"
      ? sourcePaneLabel
        ? `AI from ${sourcePaneLabel}`
        : "AI"
      : null;
    const paneHeading = !session && node.createdBy === "user" && node.launchState === "unlaunched"
      ? "Launch"
      : activeItem?.title ?? "Pane";

    return (
      <div className={`stack-pane ${node.createdBy === "ai" ? "ai-pane" : "user-pane"}`}>
        <div className="stack-toolbar">
          <div className="stack-primary">
            <span className={`pane-badge ${node.createdBy === "ai" ? "ai" : "user"}`} title={`Pane ${node.paneLabel}`}>
              {node.paneLabel}
            </span>
            {originLabel ? (
              <span className="pane-origin" title={originLabel}>
                {originLabel}
              </span>
            ) : null}
            {showStackTabs ? (
              <div className="stack-tabs">
                {node.items.map((item: StackItem) => (
                  <button
                    key={item.id}
                    className={`stack-tab ${item.id === node.activeItemId ? "active" : ""}`}
                    title={item.title}
                    onClick={() => {
                      if (!activeProjectId || !activeTab) return;
                      void refreshWorkspace(setActiveStackItem(activeProjectId, activeTab.id, node.id, item.id));
                    }}
                  >
                    {item.title}
                  </button>
                ))}
              </div>
            ) : (
              <div className="pane-label" title={paneHeading}>
                {paneHeading}
              </div>
            )}
          </div>
          <div className="stack-actions">
            {session ? (
              <span className={`status-pill toolbar-status ${session.status}`}>{session.status}</span>
            ) : null}
            <button
              className="toolbar-button"
              title="Split Up/Down"
              onClick={() => {
                if (!activeProjectId || !activeTab) return;
                void refreshWorkspace(splitPane(activeProjectId, activeTab.id, node.id, "vertical"));
              }}
            >
              ━
            </button>
            <button
              className="toolbar-button"
              title="Split Left/Right"
              onClick={() => {
                if (!activeProjectId || !activeTab) return;
                void refreshWorkspace(splitPane(activeProjectId, activeTab.id, node.id, "horizontal"));
              }}
            >
              ┃
            </button>
            {canClosePane ? (
              <button
                className="toolbar-button subtle"
                onClick={() => {
                  if (!activeProjectId || !activeTab) return;
                  void refreshWorkspace(closePane(activeProjectId, activeTab.id, node.id));
                }}
              >
                Close Pane
              </button>
            ) : null}
          </div>
        </div>
        <div className="stack-body">
          {session && session.status === "running" ? (
            <TerminalView sessionId={session.id} variant={node.createdBy === "ai" ? "ai" : "user"} />
          ) : node.createdBy === "user" && node.launchState === "unlaunched" ? (
            renderLauncher(node)
          ) : session ? (
            <div className="empty-stack">
              <p>{`${node.paneLabel} · ${session.title}`}</p>
              <p>Session is {session.status}. Start a new terminal in this pane.</p>
              <button
                className="toolbar-button primary"
                onClick={() => {
                  if (!activeProjectId || !activeTab) return;
                  void refreshWorkspace(
                    createSession({
                      projectId: activeProjectId,
                      tabId: activeTab.id,
                      stackId: node.id,
                      title: session.title,
                      program: session.program,
                      args: session.args ?? undefined,
                      launchProfile: session.launchProfile,
                    }),
                  );
                }}
              >
                Start Replacement Session
              </button>
            </div>
          ) : (
            <div className="empty-stack">
              <p>{`${node.paneLabel} · No session attached`}</p>
              <button
                className="toolbar-button primary"
                onClick={() => {
                  if (!activeProjectId || !activeTab) return;
                  void refreshWorkspace(
                    createSession({
                      projectId: activeProjectId,
                      tabId: activeTab.id,
                      stackId: node.id,
                      launchProfile: "terminal",
                    }),
                  );
                }}
              >
                Start Terminal
              </button>
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
          <div>
            <p className="eyebrow">Workspace Terminal</p>
            <h1>Projects</h1>
          </div>
          <button className="toolbar-button primary" onClick={openProjectModal}>
            Add Project
          </button>
        </div>
        {errorMessage ? <div className="error-banner">{errorMessage}</div> : null}
        <div className="project-list">
          {projects.map((project) => (
            <div
              key={project.id}
              className={`project-card ${project.id === activeProjectId ? "active" : ""}`}
            >
              <span className="project-color" style={{ background: project.color }} />
              {renamingProjectId === project.id ? (
                <div className="project-card-main project-card-main-static">
                  <div className="project-card-content">
                    <input
                      className="project-inline-input"
                      value={renamingProjectName}
                      onChange={(event) => setRenamingProjectName(event.target.value)}
                      onBlur={() => {
                        void commitRenameProject(project.id);
                      }}
                      onKeyDown={(event) => {
                        if (event.key === "Enter") {
                          event.preventDefault();
                          void commitRenameProject(project.id);
                        }
                        if (event.key === "Escape") {
                          event.preventDefault();
                          cancelRenameProject();
                        }
                      }}
                      autoFocus
                    />
                  </div>
                </div>
              ) : (
                <button
                  className="project-card-main"
                  title={`${project.name}\n${project.path}`}
                  onClick={() => void selectProject(project.id)}
                >
                  <div className="project-card-content">
                    <strong>{project.name}</strong>
                    <div className="project-path" title={project.path}>{project.path}</div>
                  </div>
                </button>
              )}
              <div className="project-card-actions">
                <button
                  className="project-action-button"
                  title="Rename project"
                  onClick={(event) => {
                    event.stopPropagation();
                    beginRenameProject(project);
                  }}
                >
                  Rename
                </button>
                <button
                  className="project-action-button danger"
                  title="Remove project from Workspace Terminal"
                  onClick={(event) => {
                    event.stopPropagation();
                    setPendingDeleteProject(project);
                  }}
                >
                  Delete
                </button>
              </div>
            </div>
          ))}
        </div>
      </aside>

      <main className="workspace">
        <header className="workspace-header">
          <div>
            <p className="eyebrow">Project Workspace</p>
            <h2>{activeProject?.name ?? "No project selected"}</h2>
          </div>
          <button
            className="toolbar-button primary"
            onClick={() => {
              if (!activeProjectId) return;
              void refreshWorkspace(createTab(activeProjectId));
            }}
          >
            New Tab
          </button>
        </header>

        <div className="workspace-tabs">
          {snapshot?.tabs.map((tab) => (
            <button
              key={tab.id}
              className={`workspace-tab ${snapshot.activeTabId === tab.id ? "active" : ""}`}
              title={tab.title}
              onClick={() => {
                if (!activeProjectId) return;
                void refreshWorkspace(setActiveTab(activeProjectId, tab.id));
              }}
              onDoubleClick={() => void handleRenameTab(tab.id, tab.title)}
            >
              <span>{tab.title}</span>
              <span
                className="workspace-tab-close"
                onClick={(event) => {
                  event.stopPropagation();
                  if (!activeProjectId) return;
                  void refreshWorkspace(closeTab(activeProjectId, tab.id));
                }}
              >
                x
              </span>
            </button>
          ))}
        </div>

        <section
          className="workspace-canvas"
          ref={workspaceCanvasRef}
          onDragOver={(event) => {
            event.preventDefault();
          }}
          onDrop={(event) => {
            event.preventDefault();
          }}
        >
          {loading ? (
            <div className="empty-state">Loading workspace...</div>
          ) : activeTab ? (
            renderNode(activeTab.root, false)
          ) : (
            <div className="empty-state">Create a project and open a tab to start.</div>
          )}
        </section>
      </main>

      {isProjectModalOpen ? (
        <div
          className="modal-backdrop"
          onClick={(event) => {
            if (event.target === event.currentTarget) {
              closeProjectModal();
            }
          }}
        >
          <div className="modal-card">
            <div className="modal-header">
              <div>
                <p className="eyebrow">New Project</p>
                <h3>Add a local workspace</h3>
              </div>
              <button className="toolbar-button" onClick={closeProjectModal}>
                Close
              </button>
            </div>
            <div className="project-form modal-form">
              <input
                placeholder="Project name"
                value={newProjectName}
                onChange={(event) => setNewProjectName(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    void submitProject();
                  }
                }}
                autoFocus
              />
              <div className="path-field">
                <input
                  placeholder="Absolute path"
                  value={newProjectPath}
                  onChange={(event) => setNewProjectPath(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      void submitProject();
                    }
                  }}
                />
                <button className="toolbar-button" onClick={() => void browseForProjectPath()}>
                  Browse
                </button>
              </div>
              <div className="modal-actions">
                <button className="toolbar-button" onClick={closeProjectModal}>
                  Cancel
                </button>
                <button className="toolbar-button primary" onClick={() => void submitProject()}>
                  Add Project
                </button>
              </div>
            </div>
          </div>
        </div>
      ) : null}

      {pendingDeleteProject ? (
        <div
          className="modal-backdrop"
          onClick={(event) => {
            if (event.target === event.currentTarget) {
              setPendingDeleteProject(null);
            }
          }}
        >
          <div className="modal-card modal-card-compact">
            <div className="modal-header">
              <div>
                <p className="eyebrow">Remove Project</p>
                <h3>{pendingDeleteProject.name}</h3>
              </div>
              <button className="toolbar-button" onClick={() => setPendingDeleteProject(null)}>
                Close
              </button>
            </div>
            <p className="modal-copy">
              This removes the project from Workspace Terminal only. The actual folder stays on disk.
            </p>
            <div className="modal-actions">
              <button className="toolbar-button" onClick={() => setPendingDeleteProject(null)}>
                Cancel
              </button>
              <button className="toolbar-button subtle" onClick={() => void confirmDeleteProject()}>
                Delete Project
              </button>
            </div>
          </div>
        </div>
      ) : null}
      </div>
    </AppErrorBoundary>
  );
}
