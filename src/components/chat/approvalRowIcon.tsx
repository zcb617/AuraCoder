import { SquareTerminal, FilePen, GitBranch, Shield } from "lucide-react";
import type { ActionType } from "../../types";

export function approvalRowIcon(actionType: ActionType) {
  switch (actionType) {
    case "command":
      return <SquareTerminal size={13} />;
    case "file_write":
    case "file_edit":
    case "file_delete":
      return <FilePen size={13} />;
    case "git":
      return <GitBranch size={13} />;
    default:
      return <Shield size={13} />;
  }
}
