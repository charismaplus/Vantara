import type { ErrorInfo, ReactNode } from "react";
import { Component } from "react";

type AppErrorBoundaryProps = {
  children: ReactNode;
};

type AppErrorBoundaryState = {
  hasError: boolean;
  message: string;
};

export class AppErrorBoundary extends Component<AppErrorBoundaryProps, AppErrorBoundaryState> {
  override state: AppErrorBoundaryState = {
    hasError: false,
    message: "",
  };

  static getDerivedStateFromError(error: Error): AppErrorBoundaryState {
    return {
      hasError: true,
      message: error.message || "Unknown render error",
    };
  }

  override componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error("Workspace Terminal render crash", error, errorInfo);
  }

  private handleReload = () => {
    window.location.reload();
  };

  override render() {
    if (this.state.hasError) {
      return (
        <div className="app-shell">
          <main className="workspace">
            <section className="workspace-canvas">
              <div className="empty-state">
                <p>The workspace view ran into an unexpected error.</p>
                <p>{this.state.message}</p>
                <button className="toolbar-button primary" onClick={this.handleReload}>
                  Reload App
                </button>
              </div>
            </section>
          </main>
        </div>
      );
    }

    return this.props.children;
  }
}
