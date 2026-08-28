import { useEffect, useState } from "react";
import { Label, Slider, SliderThumb, SliderTrack } from "react-aria-components";

import type { UsageWindow } from "../api";
import { useT, type Translate } from "../i18n";

type Props = {
  label: string;
  window: UsageWindow | null;
  threshold: number;
  /** Live while dragging, so the caption tracks the thumb. */
  onPreview: (value: number) => void;
  /** Fired once the drag ends — this is what gets persisted. */
  onCommit: (value: number) => void;
  isDisabled?: boolean;
  /** False when auto-switch is off: the threshold is still editable, but it
   *  drives nothing, so the marker fades into the background. */
  isArmed?: boolean;
};

/**
 * A consumption bar with a draggable threshold marker on it. The filled part is
 * what the plan window has spent; the thumb is where the auto-switch gives up on
 * this account.
 */
export default function UsageMeter({
  label,
  window,
  threshold,
  onPreview,
  onCommit,
  isDisabled,
  isArmed = true,
}: Props) {
  // Local state keeps the drag smooth; the prop wins whenever it changes.
  const t = useT();
  const [value, setValue] = useState(threshold);
  useEffect(() => setValue(threshold), [threshold]);

  const used = window?.utilization ?? null;
  const reached = used !== null && used >= value;

  return (
    <Slider
      className="meter"
      minValue={1}
      maxValue={100}
      step={1}
      value={value}
      isDisabled={isDisabled}
      onChange={(v) => {
        const next = v as number;
        setValue(next);
        onPreview(next);
      }}
      onChangeEnd={(v) => onCommit(v as number)}
    >
      <div className="meter-head">
        <Label className="meter-label">{label}</Label>
        <span className={`meter-value${reached ? " meter-value-hit" : ""}`}>
          {used === null ? t("usage.no_data") : `${Math.round(used)}%`}
          <span className="meter-threshold">
            {t("usage.threshold_caption", { value })}
          </span>
        </span>
      </div>

      <SliderTrack className="meter-track">
        <div className="meter-rail" />
        {used !== null && (
          <div
            className={`meter-fill${reached ? " meter-fill-hit" : ""}`}
            style={{ width: `${used}%` }}
          />
        )}
        <SliderThumb
          className={`meter-thumb${isArmed ? "" : " meter-thumb-idle"}`}
          aria-label={t("usage.threshold_label", { window: label })}
        />
      </SliderTrack>

      <p className="meter-foot">{resetLine(t, window)}</p>
    </Slider>
  );
}

function resetLine(t: Translate, window: UsageWindow | null) {
  // A non-breaking space rather than nothing: the caption keeps its line, so a
  // meter without a reset time does not make the card jump in height.
  if (!window?.resetsAt) return " ";
  const ms = Date.parse(window.resetsAt) - Date.now();
  if (Number.isNaN(ms)) return " ";
  if (ms <= 0) return t("usage.resetting");

  const minutes = Math.round(ms / 60_000);
  if (minutes < 60) return t("usage.resets_in_minutes", { minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    const rest = minutes % 60;
    return rest
      ? t("usage.resets_in_hours_minutes", { hours, minutes: rest })
      : t("usage.resets_in_hours", { hours });
  }
  const days = Math.floor(hours / 24);
  return t("usage.resets_in_days", { days, hours: hours % 24 });
}
