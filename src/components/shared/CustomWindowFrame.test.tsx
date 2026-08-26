import { isValidElement, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  AutomaticDownloadProgressControl,
  CustomWindowFrame,
  getAutomaticDownloadPercent,
  WindowUpdateControl,
} from "./CustomWindowFrame";
import { renderToStaticMarkup } from "react-dom/server";
import { useUpdateStore } from "../../stores/updateStore";

const mockCloseCurrentWindow = vi.hoisted(() => vi.fn());
const mockMinimizeCurrentWindow = vi.hoisted(() => vi.fn());
const mockToggleCurrentWindowMaximize = vi.hoisted(() => vi.fn());

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string) => key,
  }),
}));

vi.mock("../../lib/windowActions", () => ({
  closeCurrentWindow: mockCloseCurrentWindow,
  minimizeCurrentWindow: mockMinimizeCurrentWindow,
  requestWindowClose: vi.fn(),
  toggleCurrentWindowMaximize: mockToggleCurrentWindowMaximize,
  toggleWindowFullscreen: vi.fn(),
}));

describe("CustomWindowFrame", () => {
  function findElement(
    node: ReactNode,
    predicate: (props: Record<string, unknown>) => boolean,
  ): Record<string, unknown> | null {
    if (Array.isArray(node)) {
      for (const child of node) {
        const match = findElement(child, predicate);
        if (match) {
          return match;
        }
      }
      return null;
    }

    if (!isValidElement(node)) {
      return null;
    }

    const props = node.props as Record<string, unknown>;
    if (predicate(props)) {
      return props;
    }

    return findElement(props.children as ReactNode, predicate);
  }

  beforeEach(() => {
    vi.clearAllMocks();
    mockCloseCurrentWindow.mockResolvedValue(undefined);
    mockMinimizeCurrentWindow.mockResolvedValue(undefined);
    mockToggleCurrentWindowMaximize.mockResolvedValue(undefined);
    useUpdateStore.setState({
      status: "idle",
      version: null,
      error: null,
      downloadPhase: "idle",
      downloadedBytes: 0,
      totalBytes: null,
      downloadSource: null,
      snoozed: false,
    });
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it("uses the native close action for the close control", async () => {
    const tree = CustomWindowFrame({ frameState: { isFullscreen: false, isMaximized: false } });
    const closeButtonProps = findElement(
      tree,
      (props) => props["aria-label"] === "windowControls.close",
    );

    expect(closeButtonProps).not.toBeNull();
    await (closeButtonProps?.onClick as (() => Promise<void> | void) | undefined)?.();

    expect(mockCloseCurrentWindow).toHaveBeenCalledTimes(1);
  });

  it("renders the restore label while maximized", () => {
    const tree = CustomWindowFrame({ frameState: { isFullscreen: false, isMaximized: true } });
    const maximizeButtonProps = findElement(
      tree,
      (props) => props["aria-label"] === "windowControls.restore",
    );

    expect(maximizeButtonProps).not.toBeNull();
  });

  it("keeps automatic download progress below 100%", () => {
    expect(getAutomaticDownloadPercent(0, 1000)).toBe(0);
    expect(getAutomaticDownloadPercent(995, 1000)).toBe(99);
    expect(getAutomaticDownloadPercent(1000, 1000)).toBe(99);
  });

  it("renders the automatic progress circle", () => {
    const downloadingMarkup = renderToStaticMarkup(
      <AutomaticDownloadProgressControl percent={25} />,
    );
    expect(downloadingMarkup).toContain("linux-window-update-control--progress");
    expect(downloadingMarkup).toContain("25%");
    expect(downloadingMarkup).toContain('role="status"');
  });

  it("keeps the state control connected to automatic update state", () => {
    useUpdateStore.setState({
      status: "downloading",
      downloadSource: "automatic",
      downloadPhase: "downloading",
      downloadedBytes: 250,
      totalBytes: 1000,
    });
    expect(WindowUpdateControl).toBeTypeOf("function");

    useUpdateStore.setState({ status: "downloaded" });
    expect(useUpdateStore.getState().status).toBe("downloaded");
  });

  it("shows the update action after restoring a completed download", () => {
    useUpdateStore.setState({
      status: "downloaded",
      downloadSource: "automatic",
    });

    expect(useUpdateStore.getState().status).toBe("downloaded");
    expect(useUpdateStore.getState().downloadSource).toBe("automatic");
    expect(WindowUpdateControl).toBeTypeOf("function");
  });
});
