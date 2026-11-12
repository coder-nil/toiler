"use client";

import { useI18n } from "@/hooks/useI18n";
import { SelectPicker } from "./SelectPicker";

type BranchPickerProps = {
  branches: string[];
  value: string;
  disabled?: boolean;
  placement?: "above" | "below";
  placeholder?: string;
  onChange: (branch: string) => void;
};

export function BranchPicker({ branches, value, disabled = false, placement = "below", placeholder, onChange }: BranchPickerProps) {
  const { t } = useI18n();
  const label = placeholder || t("git.selectBranch");
  return <SelectPicker options={branches} value={value} disabled={disabled} placement={placement} placeholder={label} ariaLabel={label} onChange={onChange} />;
}
