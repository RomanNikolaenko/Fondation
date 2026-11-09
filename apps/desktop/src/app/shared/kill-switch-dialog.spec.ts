import { TestBed } from '@angular/core/testing';

import { EngineService } from '../core/engine.service';
import { EngineStore } from '../core/engine-store';
import { KillSwitchDialog } from './kill-switch-dialog';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

interface Setup {
  fixture: ReturnType<typeof TestBed.createComponent<KillSwitchDialog>>;
  api: { killEngine: ReturnType<typeof vi.fn> };
}

function setup(): Setup {
  const api = {
    killEngine: vi.fn().mockResolvedValue({
      broker_side: 'scheduled',
      message: 'заплановано',
    }),
  };
  const store = { refresh: vi.fn().mockResolvedValue(undefined) };

  TestBed.configureTestingModule({
    imports: [KillSwitchDialog],
    providers: [
      { provide: EngineService, useValue: api },
      { provide: EngineStore, useValue: store },
    ],
  });

  const fixture = TestBed.createComponent(KillSwitchDialog);
  fixture.detectChanges();

  const dialog = fixture.nativeElement.querySelector('dialog') as HTMLDialogElement;
  dialog.close = vi.fn();
  dialog.showModal = vi.fn(() => dialog.setAttribute('open', ''));

  return { fixture, api };
}

function acknowledge(fixture: Setup['fixture']): void {
  const checkbox = fixture.nativeElement.querySelector('.ack input') as HTMLInputElement;
  checkbox.click();
  fixture.detectChanges();
}

function stopButton(fixture: Setup['fixture']): HTMLButtonElement {
  return fixture.nativeElement.querySelector('button.danger') as HTMLButtonElement;
}

describe('KillSwitchDialog', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('stop button disabled until acknowledged', () => {
    const { fixture } = setup();

    expect(stopButton(fixture).disabled).toBe(true);

    acknowledge(fixture);

    expect(stopButton(fixture).disabled).toBe(false);
  });

  it('kill dialog sends close_positions=false by default', async () => {
    const { fixture, api } = setup();

    acknowledge(fixture);
    stopButton(fixture).click();

    await vi.waitFor(() => expect(api.killEngine).toHaveBeenCalled());
    expect(api.killEngine).toHaveBeenCalledWith(false);
  });

  it('kill dialog sends true when closing positions selected', async () => {
    const { fixture, api } = setup();

    const close = fixture.nativeElement.querySelector('input[value="close"]') as HTMLInputElement;
    close.click();
    fixture.detectChanges();
    acknowledge(fixture);
    stopButton(fixture).click();

    await vi.waitFor(() => expect(api.killEngine).toHaveBeenCalled());
    expect(api.killEngine).toHaveBeenCalledWith(true);
  });
});
