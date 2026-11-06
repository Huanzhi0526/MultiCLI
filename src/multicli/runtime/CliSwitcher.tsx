import { Terminal } from "lucide-react";
import { useTranslation } from "react-i18next";
import codexIcon from "@lobehub/icons-static-svg/icons/openai.svg?url";
import claudeIcon from "@/assets/model-icons/claude.svg";
import codebuddyIcon from "@/assets/model-icons/codebuddy.svg";
import type { RuntimeCapabilityView } from "../types";

const CORE_CLI = [
  { product: "codex", name: "Codex", icon: codexIcon },
  { product: "claude-code", name: "Claude Code", icon: claudeIcon },
  { product: "codebuddy-code", name: "CodeBuddy", icon: codebuddyIcon },
];

export function cliChoices(items: RuntimeCapabilityView[]) {
  const extras = items.filter(item => !CORE_CLI.some(core => core.product === item.product));
  return [...CORE_CLI, ...extras.filter((item, index) => extras.findIndex(other => other.product === item.product) === index)
    .map(item => ({ product: item.product, name: item.productName, icon: "" }))];
}

/** Selection names a target for a NEW run. It never changes an existing run. */
export default function CliSwitcher({ items, selected, onSelect, busy = false }: {
  items: RuntimeCapabilityView[];
  selected: string;
  onSelect: (product: string) => void;
  busy?: boolean;
}) {
  const { t } = useTranslation("multicli");
  return <div className="mc-cli-switcher" role="group" aria-label={t("cliSwitchLabel")}>
    {cliChoices(items).map(cli => {
      const available = items.some(item => item.product === cli.product && item.installationId !== null);
      const active = selected === cli.product;
      return <button type="button" key={cli.product} className={`mc-cli-choice ${active ? "is-selected" : ""}`}
        data-testid={`cli-select-${cli.product}`} aria-pressed={active} aria-label={cli.name}
        title={`${cli.name} · ${available ? t("cliRegistered") : t("runtimeNotInstalled")}`}
        disabled={busy} onClick={() => onSelect(cli.product)}>
        {cli.icon ? <img className={`mc-cli-logo mc-cli-logo-${cli.product}`} src={cli.icon} alt="" /> : <Terminal aria-hidden="true" />}
        {active ? <span>{cli.name}</span> : null}
      </button>;
    })}
  </div>;
}
