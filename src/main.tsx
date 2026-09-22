import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import "./index.css";

/** A render error in a frameless always-on-top window would otherwise leave an
 *  undraggable, unclosable rectangle on screen. */
class ErrorBoundary extends React.Component<{ children: React.ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };
  static getDerivedStateFromError(error: Error) {
    return { error };
  }
  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error("render error", error, info.componentStack);
  }
  render() {
    if (!this.state.error) return this.props.children;
    const win = getCurrentWindow();
    return (
      <div className="flex h-full flex-col bg-bg text-fg">
        <div data-tauri-drag-region className="flex items-center gap-2 border-b border-line px-2.5 py-1 text-dim">
          <span data-tauri-drag-region className="flex-1">
            soltrack — something went wrong
          </span>
          <button className="rounded px-1.5 hover:bg-line" onClick={() => window.location.reload()} title="Reload">
            ↻
          </button>
          <button className="rounded px-1.5 text-down hover:bg-line" onClick={() => win.close()} title="Close">
            ✕
          </button>
        </div>
        <pre className="selectable overflow-auto p-2.5 text-[0.85rem] text-down">{String(this.state.error)}</pre>
      </div>
    );
  }
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
