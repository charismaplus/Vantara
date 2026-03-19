import type { ReactNode } from "react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Group as PanelGroup, Panel, Separator as PanelResizeHandle } from "react-resizable-panels";
import type {
  LayoutNode,
  LaunchProfile,
  Project,
  ProjectWorkspaceSnapshot,
  TerminalSession,
  WorkspaceSession,
  WorkspaceSnapshot,
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
  listProjects,
  openProject,
  openSession,
  renameProject,
  renameWindow,
  renameWorkspaceSession,
  reportTabViewport,
  setActiveWindow,
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

export default function App() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [activeProjectId, setActiveProjectId] = useState<string | null>(null);
  const [projectSnapshot, setProjectSnapshot] = useState<ProjectWorkspaceSnapshot | null>(null);
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
  const [pendingDeleteSession, setPendingDeleteSession] = useState<WorkspaceSession | null>(null);
  const workspaceCanvasRef = useRef<HTMLElement | null>(null);

  const activeProject = useMemo(() => projects.find((entry) => entry.id === activeProjectId) ?? null, [projects, activeProjectId]);
  const activeWorkspaceSession = useMemo(
    () => projectSnapshot?.sessions.find((entry) => entry.id === activeWorkspaceSessionId) ?? null,
    [projectSnapshot, activeWorkspaceSessionId],
  );
  const sessions = useMemo(() => createSessionMap(sessionSnapshot), [sessionSnapshot]);
  const activeWindow = useMemo(
    () => getActiveTab(sessionSnapshot?.windows ?? [], sessionSnapshot?.activeWindowId),
    [sessionSnapshot],
  );
  const paneLabels = useMemo(() => {
    const labels = new Map<string, string>();
    if (activeWindow) {
      collectPaneLabels(activeWindow.root, labels);
    }
    return labels;
  }, [activeWindow]);

  useEffect(() => {
    void (async () => {
      try {
        const loadedProjects = await listProjects();
        setProjects(loadedProjects);
        if (loadedProjects[0]) {
          setExpandedProjectIds({ [loadedProjects[0].id]: true });
          setLoading(true);
          setActiveProjectId(loadedProjects[0].id);
          setActiveWorkspaceSessionId(null);
          setSessionSnapshot(null);
          setProjectSnapshot(await openProject(loadedProjects[0].id));
          setProjects(await listProjects());
          setLoading(false);
        }
      } catch (error) {
        showError(error);
        setLoading(false);
      }
    })();
  }, []);

  useEffect(() => {
    const disposers: Array<() => void> = [];
    const refreshCurrent = () => {
      if (!activeProjectId) {
        return;
      }
      void (async () => {
        setLoading(true);
        try {
          const nextProjectSnapshot = await openProject(activeProjectId);
          setProjectSnapshot(nextProjectSnapshot);
          setProjects(await listProjects());
          if (activeWorkspaceSessionId && nextProjectSnapshot.sessions.some((entry) => entry.id === activeWorkspaceSessionId)) {
            setSessionSnapshot(await openSession(activeProjectId, activeWorkspaceSessionId));
          } else if (activeWorkspaceSessionId) {
            setActiveWorkspaceSessionId(null);
            setSessionSnapshot(null);
          }
        } catch (error) {
          showError(error);
        } finally {
          setLoading(false);
        }
      })();
    };

    void listenSessionOutput((event) => writeTerminalChunk(event.payload.sessionId, event.payload.chunk)).then((dispose) => disposers.push(dispose));

    void listenSessionExit(() => {
      refreshCurrent();
    }).then((dispose) => disposers.push(dispose));

    void listenWorkspaceChanged((event) => {
      if (!activeProjectId || event.payload.projectId !== activeProjectId) {
        return;
      }
      if (event.payload.sessionId && activeWorkspaceSessionId && event.payload.sessionId !== activeWorkspaceSessionId) {
        void (async () => {
          try {
            const nextProjectSnapshot = await openProject(activeProjectId);
            setProjectSnapshot(nextProjectSnapshot);
            setProjects(await listProjects());
          } catch (error) {
            showError(error);
          }
        })();
        return;
      }
      refreshCurrent();
    }).then((dispose) => disposers.push(dispose));

    return () => {
      for (const dispose of disposers) {
        dispose();
      }
    };
  }, [activeProjectId, activeWorkspaceSessionId]);

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
    }).then((unlisten) => {
      dispose = unlisten;
    });
    return () => dispose?.();
  }, []);

  async function refreshProjectOnly(projectId: string) {
    const nextProjectSnapshot = await openProject(projectId);
    setProjectSnapshot(nextProjectSnapshot);
    const nextProjects = await listProjects();
    setProjects(nextProjects);
    if (activeWorkspaceSessionId && !nextProjectSnapshot.sessions.some((entry) => entry.id === activeWorkspaceSessionId)) {
      setActiveWorkspaceSessionId(null);
      setSessionSnapshot(null);
    }
  }

  async function selectProject(projectId: string) {
    setLoading(true);
    setErrorMessage(null);
    try {
      setActiveProjectId(projectId);
      setActiveWorkspaceSessionId(null);
      setSessionSnapshot(null);
      setExpandedProjectIds((current) => ({ ...current, [projectId]: true }));
      setProjectSnapshot(await openProject(projectId));
      setProjects(await listProjects());
    } catch (error) {
      showError(error);
    } finally {
      setLoading(false);
    }
  }

  async function selectWorkspaceSession(projectId: string, workspaceSessionId: string) {
    setLoading(true);
    setErrorMessage(null);
    try {
      setActiveProjectId(projectId);
      setActiveWorkspaceSessionId(workspaceSessionId);
      setExpandedProjectIds((current) => ({ ...current, [projectId]: true }));
      const [nextProjectSnapshot, nextSessionSnapshot, nextProjects] = await Promise.all([
        openProject(projectId),
        openSession(projectId, workspaceSessionId),
        listProjects(),
      ]);
      setProjectSnapshot(nextProjectSnapshot);
      setSessionSnapshot(nextSessionSnapshot);
      setProjects(nextProjects);
    } catch (error) {
      showError(error);
    } finally {
      setLoading(false);
    }
  }

  async function refreshSessionSnapshot(promise: Promise<WorkspaceSnapshot>) {
    try {
      setErrorMessage(null);
      const nextSnapshot = await promise;
      setSessionSnapshot(nextSnapshot);
      if (activeProjectId) {
        setProjectSnapshot(await openProject(activeProjectId));
      }
    } catch (error) {
      showError(error);
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

  async function launchFromSessionSurface(profile: LauncherProfile) {
    if (!activeProjectId || !activeWorkspaceSessionId) {
      return;
    }
    await refreshSessionSnapshot(createSession({
      projectId: activeProjectId,
      workspaceSessionId: activeWorkspaceSessionId,
      title: profile.sessionTitle,
      program: profile.program,
      args: profile.args,
      launchProfile: profile.launchProfile,
    }));
  }

  function showError(error: unknown) {
    setErrorMessage(error instanceof Error ? error.message : String(error));
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

    return (
      <div className={`stack-pane ${node.createdBy === "ai" ? "ai-pane" : "user-pane"}`}>
        <div className="stack-toolbar">
          <div className="stack-primary">
            <span className={`pane-badge ${node.createdBy === "ai" ? "ai" : "user"}`}>{node.paneLabel}</span>
            {originLabel ? <span className="pane-origin">{originLabel}</span> : null}
            <div className="pane-label" title={activeItem?.title ?? "Pane"}>{activeItem?.title ?? "Pane"}</div>
          </div>
          <div className="stack-actions">
            {session ? <span className={`status-pill toolbar-status ${session.status}`}>{session.status}</span> : null}
            <button className="toolbar-button" title="Split Up/Down" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(splitPane(activeProjectId, activeWorkspaceSessionId, activeWindow.id, node.id, "vertical"))}>━</button>
            <button className="toolbar-button" title="Split Left/Right" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(splitPane(activeProjectId, activeWorkspaceSessionId, activeWindow.id, node.id, "horizontal"))}>┃</button>
            {canClosePane ? <button className="toolbar-button subtle" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(closePane(activeProjectId, activeWorkspaceSessionId, activeWindow.id, node.id))}>Close Pane</button> : null}
          </div>
        </div>
        <div className="stack-body">
          {session && session.status === "running" ? (
            <TerminalView sessionId={session.id} variant={node.createdBy === "ai" ? "ai" : "user"} />
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
            <div><p className="eyebrow">Workspace Terminal</p><h1>Projects</h1></div>
            <button className="toolbar-button primary" onClick={() => setIsProjectModalOpen(true)}>Add Project</button>
          </div>
          {errorMessage ? <div className="error-banner">{errorMessage}</div> : null}
          <div className="project-list">
            {projects.map((project) => {
              const projectSessions = projectSnapshot?.projectId === project.id ? projectSnapshot.sessions : [];
              return (
                <div key={project.id} className={`project-card ${project.id === activeProjectId ? "active" : ""}`}>
                  <span className="project-color" style={{ background: project.color }} />
                  {renamingProjectId === project.id ? (
                    <div className="project-card-main project-card-main-static">
                      <div className="project-card-content">
                        <input className="project-inline-input" value={renamingProjectName} onChange={(event) => setRenamingProjectName(event.target.value)} onBlur={() => renameProject(project.id, renamingProjectName.trim()).then(async () => { setRenamingProjectId(null); await refreshProjectOnly(project.id); }).catch(showError)} autoFocus />
                      </div>
                    </div>
                  ) : (
                    <div>
                      <div className="project-card-main" onClick={() => void selectProject(project.id)}>
                        <div className="project-card-content"><strong>{project.name}</strong><div className="project-path">{project.path}</div></div>
                      </div>
                      <div className="project-card-actions">
                        <button className="project-action-button" onClick={() => setExpandedProjectIds((current) => ({ ...current, [project.id]: !current[project.id] }))}>{expandedProjectIds[project.id] ? "Collapse" : "Expand"}</button>
                        <button className="project-action-button" onClick={() => { setRenamingProjectId(project.id); setRenamingProjectName(project.name); }}>Rename</button>
                        <button className="project-action-button danger" onClick={() => setPendingDeleteProject(project)}>Delete</button>
                      </div>
                      {expandedProjectIds[project.id] ? (
                        <div className="project-list" style={{ paddingLeft: 12 }}>
                          {projectSessions.map((session) => (
                            <div key={session.id} className={`project-card ${session.id === activeWorkspaceSessionId ? "active" : ""}`}>
                              <button className="project-card-main" onClick={() => void selectWorkspaceSession(project.id, session.id)}>
                                <div className="project-card-content">
                                  <strong>{session.name}</strong>
                                  <div className="project-path">{session.createdBy === "ai" ? "AI Session" : "User Session"}</div>
                                </div>
                              </button>
                              <div className="project-card-actions">
                                <button className="project-action-button" onClick={() => { const nextName = window.prompt("Rename session", session.name); if (nextName?.trim()) { void renameWorkspaceSession(project.id, session.id, nextName.trim()).then(() => refreshProjectOnly(project.id)).catch(showError); } }}>Rename</button>
                                <button className="project-action-button danger" onClick={() => setPendingDeleteSession(session)}>Delete</button>
                              </div>
                            </div>
                          ))}
                        </div>
                      ) : null}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </aside>

        <main className="workspace">
          <header className="workspace-header">
            <div>
              <p className="eyebrow">{activeWorkspaceSession ? "Session Workspace" : "Project Sessions"}</p>
              <h2>{activeWorkspaceSession?.name ?? activeProject?.name ?? "No project selected"}</h2>
            </div>
            {activeProjectId ? (
              activeWorkspaceSession ? (
                <button className="toolbar-button primary" onClick={() => void refreshSessionSnapshot(createWindow(activeProjectId, activeWorkspaceSession.id))}>New Window</button>
              ) : (
                <button className="toolbar-button primary" onClick={() => createWorkspaceSession(activeProjectId).then((session) => selectWorkspaceSession(activeProjectId, session.id)).catch(showError)}>New Session</button>
              )
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
              <section className="workspace-canvas" ref={workspaceCanvasRef} onDragOver={(event) => event.preventDefault()} onDrop={(event) => event.preventDefault()}>
                {loading ? <div className="empty-state">Loading workspace...</div> : activeWindow ? renderNode(activeWindow.root, false) : (
                  <div className="empty-state">
                    <p>This session has no windows yet.</p>
                    <div style={{ marginBottom: 16 }}>
                      <button className="toolbar-button primary" onClick={() => activeProjectId && void refreshSessionSnapshot(createWindow(activeProjectId, activeWorkspaceSession.id))}>New Window</button>
                    </div>
                    {renderLauncherSurface((profile) => launchFromSessionSurface(profile))}
                  </div>
                )}
              </section>
            </>
          ) : (
            <section className="workspace-canvas">
              {loading ? <div className="empty-state">Loading project...</div> : activeProject ? (
                <div className="project-list">
                  {(projectSnapshot?.sessions ?? []).map((session) => (
                    <div key={session.id} className="project-card">
                      <button className="project-card-main" onClick={() => void selectWorkspaceSession(activeProject.id, session.id)}>
                        <div className="project-card-content">
                          <strong>{session.name}</strong>
                          <div className="project-path">{session.createdBy === "ai" ? "AI generated session" : "User session"}</div>
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

        {pendingDeleteProject ? <div className="modal-backdrop" onClick={(event) => event.target === event.currentTarget && setPendingDeleteProject(null)}><div className="modal-card modal-card-compact"><div className="modal-header"><div><p className="eyebrow">Remove Project</p><h3>{pendingDeleteProject.name}</h3></div><button className="toolbar-button" onClick={() => setPendingDeleteProject(null)}>Close</button></div><p className="modal-copy">This removes the project from Workspace Terminal only. The actual folder stays on disk.</p><div className="modal-actions"><button className="toolbar-button" onClick={() => setPendingDeleteProject(null)}>Cancel</button><button className="toolbar-button subtle" onClick={() => deleteProject(pendingDeleteProject.id).then(async (result) => { setPendingDeleteProject(null); const nextProjects = await listProjects(); setProjects(nextProjects); if (result.nextProjectId) { await selectProject(result.nextProjectId); } else { setActiveProjectId(null); setProjectSnapshot(null); setActiveWorkspaceSessionId(null); setSessionSnapshot(null); } }).catch(showError)}>Delete Project</button></div></div></div> : null}

        {pendingDeleteSession && activeProjectId ? <div className="modal-backdrop" onClick={(event) => event.target === event.currentTarget && setPendingDeleteSession(null)}><div className="modal-card modal-card-compact"><div className="modal-header"><div><p className="eyebrow">Remove Session</p><h3>{pendingDeleteSession.name}</h3></div><button className="toolbar-button" onClick={() => setPendingDeleteSession(null)}>Close</button></div><p className="modal-copy">This removes the session node and its runtime terminals. The project remains intact.</p><div className="modal-actions"><button className="toolbar-button" onClick={() => setPendingDeleteSession(null)}>Cancel</button><button className="toolbar-button subtle" onClick={() => deleteWorkspaceSession(activeProjectId, pendingDeleteSession.id).then((nextSnapshot) => { setPendingDeleteSession(null); setProjectSnapshot(nextSnapshot); if (activeWorkspaceSessionId === pendingDeleteSession.id) { setActiveWorkspaceSessionId(null); setSessionSnapshot(null); } }).catch(showError)}>Delete Session</button></div></div></div> : null}
      </div>
    </AppErrorBoundary>
  );
}
