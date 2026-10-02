import { useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { saveConfig } from '../lib/api';
import { fmtRange } from '../lib/format';
import type { AsicBounds, MinerConfig, Range, Tuning } from '../lib/types';
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
}

export default function ConfigMining({ miner, bounds, defaultEfficiency }: ConfigMiningProps) {
  const fixed = miner?.tuning.Fixed;
  const power = miner?.tuning.Power;
  const [mode, setMode] = useState<TunerMode>(() => (power ? 'Power' : 'Fixed'));
  const [freqMhz, setFreqMhz] = useState<string | number>(() =>
    fixed ? fixed.frequency_khz / 1000 : '',
  );
  const [voltV, setVoltV] = useState<string | number>(() => (fixed ? fixed.voltage_mv / 1000 : ''));
  const [inputPowerW, setInputPowerW] = useState<string | number>(() =>
    power ? power.input_power_mw / 1000 : '',
  );
  // The board's default is offered until the operator stores their own.
  const [efficiencyJPerTh, setEfficiencyJPerTh] = useState<string | number>(() => {
    const mjPerTh = power ? power.efficiency_mj_per_th : defaultEfficiency;
    return mjPerTh == null ? '' : mjPerTh / 1000;
  });

  const freq = scaled(bounds.frequencyKhz);
  const volt = scaled(bounds.voltageMv);
  const isPower = mode === 'Power';

  // Every field is required: a switch of mode can show the other mode's
  // fields empty, and an empty number input posts as zero, which is a
  // target nobody entered.
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
              <button type="button" class={!isPower ? 'active' : ''} onClick={() => setMode('Fixed')}>
                Fixed
              </button>
              <button type="button" class={isPower ? 'active' : ''} onClick={() => setMode('Power')}>
                Power
              </button>
            </span>
          </span>
        </div>
        {isPower ? (
          <>
          <Field
            label="Input power"
            unit="W"
            value={inputPowerW}
            onInput={(e) => setInputPowerW(e.currentTarget.value)}
            type="number"
            step="0.001"
            min="0"
            required
          />
          <Field
            label="Efficiency"
            unit="J/TH"
            value={efficiencyJPerTh}
            onInput={(e) => setEfficiencyJPerTh(e.currentTarget.value)}
            type="number"
            step="0.001"
            min="0.001"
            required
          />
          </>
        ) : (
          <>
          <Field
            label="Frequency"
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
          <Field
            label="Voltage"
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
          </>
        )}
      </div>
      <button type="submit">Save</button>
    </form>
    </>
  );
}
