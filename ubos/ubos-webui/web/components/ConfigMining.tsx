import { useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { saveConfig } from '../lib/api';
import { fmtRange } from '../lib/format';
import type { AsicBounds, DefaultTuning, MinerConfig, Range, Tuning } from '../lib/types';
import Field from './Field';

type TunerMode = 'Fixed' | 'Power';

// A bounds range the board states in its milli-unit, restated in the
// whole unit the form shows. It is absent until the bounds endpoint
// answers, and the field then renders with no range at all rather than
// a half-filled one.
const scaled = (range?: Range): Partial<Range> =>
  range ? { start: range.start / 1000, end: range.end / 1000 } : {};

// A field's value in the milli-unit the board stores. The round is for
// binary float error and not for the input's step grid, which is
// already a thousandth: `1.26 * 1000` is 1260.0000000000002.
const milli = (whole: string | number): number => Math.round(+whole * 1000);

export interface ConfigMiningProps {
  miner?: MinerConfig;
  bounds: Partial<AsicBounds>;
  /** The board's default efficiency in mJ/TH, as `/api/system/asic` reports it. */
  defaultEfficiency?: number;
  /** The tuning a new config starts on, as `/api/system/asic` reports it. */
  defaultTuning?: DefaultTuning;
}

export default function ConfigMining({ miner, bounds, defaultEfficiency, defaultTuning }: ConfigMiningProps) {
  const fixed = miner?.tuning.Fixed;
  const power = miner?.tuning.Power;
  const tuners = bounds.tuners ?? [];
  const fixedTuner = tuners.find((tuner) => tuner.Fixed)?.Fixed;
  const powerTuner = tuners.find((tuner) => tuner.Power)?.Power;
  const [mode, setMode] = useState<TunerMode>(() => (power ? 'Power' : 'Fixed'));
  const [freqMhz, setFreqMhz] = useState<string | number>(() =>
    fixed ? fixed.frequency_khz / 1000 : '',
  );
  const [voltV, setVoltV] = useState<string | number>(() => (fixed ? fixed.voltage_mv / 1000 : ''));
  // The machine's default power target is offered until the operator
  // stores their own.
  const [inputPowerW, setInputPowerW] = useState<string | number>(() => {
    const defaultMw = typeof defaultTuning === 'object' ? defaultTuning.Power.inputPowerMw : undefined;
    const mw = power ? power.input_power_mw : defaultMw;
    return mw == null ? '' : mw / 1000;
  });
  // The board's default is offered until the operator stores their own.
  const [efficiencyJPerTh, setEfficiencyJPerTh] = useState<string | number>(() => {
    const mjPerTh = power ? power.efficiency_mj_per_th : defaultEfficiency;
    return mjPerTh == null ? '' : mjPerTh / 1000;
  });

  const freq = scaled(bounds.frequencyKhz);
  const volt = scaled(bounds.voltageMv);
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
            unit="W"
            value={inputPowerW}
            onInput={(e) => setInputPowerW(e.currentTarget.value)}
            type="number"
            step="0.001"
            min="0"
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
