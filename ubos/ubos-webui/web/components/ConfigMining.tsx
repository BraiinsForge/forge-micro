import { useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { saveConfig } from '../lib/api';
import { fmtRange } from '../lib/format';
import type { AsicBounds, AsicDefaults, MinerConfig, Range, Tuning } from '../lib/types';
import Field from './Field';

type TunerMode = 'Fixed' | 'Power';

// A bounds range the board states in its milli-unit, restated in the
// whole unit the form shows. It is absent until the bounds endpoint
// answers, and the field then renders with no range at all rather than
// a half-filled one.
const scaled = (range?: Range | null): Partial<Range> =>
  range ? { start: range.start / 1000, end: range.end / 1000 } : {};

// A value the board states in its milli-unit, as the field's text in
// the whole unit, or empty when the board states none.
const fromMilli = (milliValue?: number): string | number => (milliValue == null ? '' : milliValue / 1000);

// A field's value in the milli-unit the board stores. The round is for
// binary float error and not for the input's step grid, which is
// already a thousandth: `1.26 * 1000` is 1260.0000000000002.
const milli = (whole: string | number): number => Math.round(+whole * 1000);

export interface ConfigMiningProps {
  miner?: MinerConfig;
  bounds: Partial<AsicBounds>;
  defaults: AsicDefaults;
}

export default function ConfigMining({ miner, bounds, defaults }: ConfigMiningProps) {
  const fixed = miner?.tuning.Fixed;
  const power = miner?.tuning.Power;
  const tuners = bounds.tuners ?? [];
  const fixedTuner = tuners.find((tuner) => tuner.Fixed)?.Fixed;
  const powerTuner = tuners.find((tuner) => tuner.Power)?.Power;
  const [mode, setMode] = useState<TunerMode>(() => (power ? 'Power' : 'Fixed'));
  // Each field offers the board's default until the operator stores
  // their own. The board states no default input power unless it
  // starts on a power target, so that field may start empty.
  const [freqMhz, setFreqMhz] = useState(() => fromMilli(fixed?.frequency_khz ?? defaults.defaultFrequencyKhz));
  const [voltV, setVoltV] = useState(() => fromMilli(fixed?.voltage_mv ?? defaults.defaultVoltageMv));
  const [inputPowerW, setInputPowerW] = useState(() =>
    fromMilli(
      power?.input_power_mw ??
        (typeof defaults.defaultTuning === 'object' ? defaults.defaultTuning.Power.inputPowerMw : undefined),
    ),
  );
  const [efficiencyJPerTh, setEfficiencyJPerTh] = useState(() =>
    fromMilli(power?.efficiency_mj_per_th ?? defaults.defaultEfficiencyMjPerTh),
  );

  const freq = scaled(bounds.frequencyKhz);
  const volt = scaled(bounds.voltageMv);
  const inputPower = scaled(bounds.inputPowerMw);
  const isPower = mode === 'Power';

  // The selected tuner's fields are required and range-checked,
  // because an empty number input posts as zero, which is a target
  // nobody entered. The other tuner's fields stay on the page dimmed
  // and disabled, so the layout holds across a mode switch while the
  // browser keeps a field the save does not carry out of both its
  // validation and the submit. A field no offered tuner reads is not
  // shown. The efficiency is posted even then, because the power
  // target requires one.
  const save = (event: JSX.TargetedEvent<HTMLFormElement, SubmitEvent>) => {
    const tuning: Tuning = isPower
      ? {
          Power: {
            input_power_mw: milli(inputPowerW),
            efficiency_mj_per_th: milli(efficiencyJPerTh),
          },
        }
      : { Fixed: { frequency_khz: milli(freqMhz), voltage_mv: milli(voltV) } };
    return saveConfig({ miner: { tuning } }, event);
  };

  return (
    <>
    <h2 class="cfgTitle">Mining</h2>
    <form onSubmit={save}>
      <div class="fldRows">
        <div class="fld">
          <span class="fldText">
            <span class="fldLabel">Tuner</span>
          </span>
          <span class="fldCtl">
            <span class="seg">
              {tuners.map((tuner) => {
                const tunerMode: TunerMode = tuner.Fixed ? 'Fixed' : 'Power';
                return (
                  <button
                    key={tunerMode}
                    type="button"
                    class={mode === tunerMode ? 'active' : ''}
                    onClick={() => setMode(tunerMode)}
                  >
                    {(tuner.Fixed ?? tuner.Power)?.name}
                  </button>
                );
              })}
            </span>
          </span>
        </div>
        {fixedTuner ? (
          <Field
            label="Frequency"
            off={isPower}
            disabled={isPower}
            desc={fmtRange(freq.start, freq.end)}
            unit="MHz"
            value={freqMhz}
            onInput={(e) => setFreqMhz(e.currentTarget.value)}
            type="number"
            step="0.001"
            min={freq.start}
            max={freq.end}
            required
          />
        ) : null}
        {fixedTuner ? (
          <Field
            label="Voltage"
            off={isPower}
            disabled={isPower}
            desc={fmtRange(volt.start, volt.end)}
            unit="V"
            value={voltV}
            onInput={(e) => setVoltV(e.currentTarget.value)}
            type="number"
            step="0.001"
            min={volt.start}
            max={volt.end}
            required
          />
        ) : null}
        {powerTuner ? (
          <Field
            label="Input power"
            off={!isPower}
            disabled={!isPower}
            desc={fmtRange(inputPower.start, inputPower.end)}
            unit="W"
            value={inputPowerW}
            onInput={(e) => setInputPowerW(e.currentTarget.value)}
            type="number"
            step="0.001"
            min={inputPower.start}
            max={inputPower.end}
            required
          />
        ) : null}
        {powerTuner?.readsEfficiency ? (
          <Field
            label="Efficiency"
            off={!isPower}
            disabled={!isPower}
            unit="J/TH"
            value={efficiencyJPerTh}
            onInput={(e) => setEfficiencyJPerTh(e.currentTarget.value)}
            type="number"
            step="0.001"
            min="0.001"
            required
          />
        ) : null}
      </div>
      <button type="submit">Save</button>
    </form>
    </>
  );
}
