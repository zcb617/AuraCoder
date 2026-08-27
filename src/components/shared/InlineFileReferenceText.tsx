import { Fragment, useMemo, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { findFileReferenceMatches } from "../../lib/fileReferences";
import {
  handleEditorFileReferenceClick,
  type EditorFileReferenceContext,
} from "../../lib/openEditorFileReference";

import { useChatComposerStore } from "../../stores/chatComposerStore";
interface InlineFileReferenceTextProps extends EditorFileReferenceContext {
  text: string;
}

export function InlineFileReferenceText({
  text,
  workspaceId,
}: InlineFileReferenceTextProps) {
  const { t } = useTranslation("common");
  const matches = useMemo(() => findFileReferenceMatches(text), [text]);
  const linkOpenGesture = useChatComposerStore((state) => state.linkOpenGesture);

  if (matches.length === 0) {
    return <>{text}</>;
  }

  const hint = t(linkOpenGesture === "click" ? "fileReferences.clickHint" : "fileReferences.shiftClickHint");
  const context = {
    workspaceId,
  };

  const nodes: ReactNode[] = [];
  let cursor = 0;

  for (const match of matches) {
    if (cursor < match.start) {
      nodes.push(
        <Fragment key={`text:${cursor}`}>
          {text.slice(cursor, match.start)}
        </Fragment>,
      );
    }
    nodes.push(
      <a
        key={`ref:${match.start}`}
        href={match.rawReference}
        title={hint}
        onClick={(event) => handleEditorFileReferenceClick(event, match.rawReference, context)}
        style={{
          color: "var(--accent)",
          textDecoration: "none",
        }}
      >
        {match.rawReference}
      </a>,
    );
    cursor = match.end;
  }

  if (cursor < text.length) {
    nodes.push(
      <Fragment key={`text:${cursor}`}>
        {text.slice(cursor)}
      </Fragment>,
    );
  }

  return <>{nodes}</>;
}
