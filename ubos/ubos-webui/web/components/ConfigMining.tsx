import { useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { saveConfig } from '../lib/api';
import { fmtRange } from '../lib/format';
import type { AsicBounds, MinerConfig, Range, Tuning } from '../lib/types';
import Field from './Field';

type TunerMode = 'Fixed' | 'Power';

export interface ConfigMiningProps {
  miner?: MinerConfig;
  bounds: Partial<AsicBounds>;
  defaultEfficiency?: number;
}

export default function ConfigMining({ miner, bounds, defaultEfficiency }: ConfigMiningProps) {
  const fixed = miner?.tuning.Fixed;
  const power = miner?.tuning.Power;
  const [mode, setMode] = useState<TunerMode>(() => (power ? 'Power' : 'Fixed'));
  const [freqMhz, setFreqMhz] = useState<string | number>(() => (fixed ? fixed.frequency_mhz : ''));
  const [voltMv, setVoltMv] = useState<string | number>(() => (fixed ? fixed.voltage_mv : ''));
  const [inputPowerMw, setInputPowerMw] = useState<string | number>(() =>
    power ? power.input_power_mw : '',
  );
  // The board's default is offered until the operator stores their own.
  const [efficiencyMjPerTh, setEfficiencyMjPerTh] = useState<string | number>(() =>
    power ? power.efficiency_mj_per_th : (defaultEfficiency ?? ''),
  );

  const freq: Partial<Range> = bounds.frequencyMhz || {};
  const volt: Partial<Range> = bounds.voltageMv || {};
  const isPower = mode === 'Power';

  // Every field is required: a switch of mode can show the other mode's
  // fields empty, and an empty number input posts as zero, which is a
  // target nobody entered.
  const save = (event: JSX.TargetedEvent<HTMLFormElement, SubmitEvent>) => {
    const tuning: Tuning = isPower
      ? { Power: { input_power_mw: +inputPowerMw, efficiency_mj_per_th: +efficiencyMjPerTh } }
      : { Fixed: { frequency_mhz: +freqMhz, voltage_mv: +voltMv } };
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
            unit="mW"
            value={inputPowerMw}
            onInput={(e) => setInputPowerMw(e.currentTarget.value)}
            type="number"
            step="1"
            min="0"
            required
          />
          <Field
            label="Efficiency"
            unit="mJ/TH"
            value={efficiencyMjPerTh}
            onInput={(e) => setEfficiencyMjPerTh(e.currentTarget.value)}
            type="number"
            step="1"
            min="1"
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
            step="1"
            min={freq.start}
            max={freq.end}
            required
          />
          <Field
            label="Voltage"
            desc={fmtRange(volt.start, volt.end)}
            unit="mV"
            value={voltMv}
            onInput={(e) => setVoltMv(e.currentTarget.value)}
            type="number"
            step="1"
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
