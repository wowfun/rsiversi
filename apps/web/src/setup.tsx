import { useLayoutEffect } from "react";
import { RegisteredSettings } from "./settings/registered.tsx";
import { Plugins } from "./plugins.tsx";
import { Button } from "./button.tsx";
import {
  input,
  useView,
  settingsMounted,
  settingsPanel,
  type SettingsSection,
} from "./bridge.ts";
import { ModelsSettings } from "./settings/models.tsx";
import { ConversationDefaults } from "./settings/conversation-defaults.tsx";
import { useSetupActions } from "./settings/setup-actions.ts";
import {
  IconCloseOutlineRegular,
  IconPersonalizationOutlineMedium,
  IconDataOutlineMedium,
  IconPluginPinwheelOutlineMedium,
  IconSlidersTwoOutlineMedium,
} from "../vendor/dsh/dockkit-primitives/icons/index.tsx";
import css from "./settings/settings.module.css";

const sections = {
  appearance: "General",
  models: "Models",
  plugins: "Plugins",
  advanced: "Advanced",
} satisfies Record<SettingsSection, string>;
const icons = {
  appearance: IconPersonalizationOutlineMedium,
  models: IconDataOutlineMedium,
  plugins: IconPluginPinwheelOutlineMedium,
  advanced: IconSlidersTwoOutlineMedium,
};

export function Setup({
  close,
  tab,
}: {
  close: () => void;
  tab: SettingsSection;
}) {
  const actions = useSetupActions();
  const receipts = useView((view) => view?.setup?.receipts),
    contributions = useView((view) => view?.application_surfaces);
  useLayoutEffect(() => {
    settingsMounted.set(true);
    return () => settingsMounted.set(false);
  }, []);
  const select = (section: SettingsSection) => {
    settingsPanel.set(section);
    if (section === "plugins")
      actions.act(() =>
        input.command({ action: "plugins", command: { kind: "refresh" } }),
      );
  };
  return (
    <section className={`setup-panel ${css.panel} ${css.scope}`} aria-label="Settings">
      <h1 className={css.title}>Settings</h1>
      <nav
        className={`settings-tabs ${css.nav}`}
        aria-label="Settings sections"
      >
        {(Object.entries(sections) as [SettingsSection, string][]).map(
          ([section, label]) => {
            const Icon = icons[section];
            return (
              <Button
                key={section}
                className={css.navCell}
                aria-pressed={tab === section}
                onClick={() => select(section)}
              >
                <Icon size={16} />
                {label}
              </Button>
            );
          },
        )}
      </nav>
      <header className={css.header}>
        <Button onClick={close} aria-label="Close settings">
          <IconCloseOutlineRegular size={20} />
        </Button>
      </header>
      <div className={`settings-options ${css.options}`}>
        <div
          className="registered-settings"
          hidden={tab !== "appearance" && tab !== "advanced"}
        >
          <RegisteredSettings />
        </div>
        <div hidden={tab !== "appearance"}>
          <ConversationDefaults actions={actions} />
        </div>
        <div hidden={tab !== "models"}>
          <ModelsSettings actions={actions} />
        </div>
        {tab === "plugins" && <Plugins loading={actions.busy} />}
        {tab === "appearance" && receipts?.some(receipt=>receipt.outcome!=='confirmed') && (
          <section className="setup-receipts" aria-label="Setup receipts">
            {receipts.filter(receipt=>receipt.outcome!=='confirmed').slice(-2).map((receipt, index) => (
              <p key={index} data-outcome={receipt.outcome}>
                <strong>
                  {receipt.operation} · {receipt.outcome}
                </strong>{" "}
                — {receipt.message}
              </p>
            ))}
          </section>
        )}
        {tab === "advanced" && !!contributions?.length && (
          <section className="application-extensions">
            <h2>Application extensions</h2>
            {contributions.map((item) => (
              <Button
                key={JSON.stringify(item.reference)}
                size="sm"
                onClick={() =>
                  actions.act(async () => {
                    await input.command({
                      action: "application_ui_surface",
                      reference: item.reference,
                    });
                    close();
                  })
                }
              >
                {item.title}
              </Button>
            ))}
          </section>
        )}
      </div>
    </section>
  );
}
