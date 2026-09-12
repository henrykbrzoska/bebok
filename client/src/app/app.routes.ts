import { Routes } from '@angular/router';

import { StartView } from '../views/start/start';
import { ChatView } from '../views/chat/chat';
import { SettingsView } from '../views/settings/settings';
import { TerminalView } from '../views/terminal/terminal';
import { ExplorerView } from '../views/explorer/explorer';
import { DebugView } from '../views/debug/debug';
import { StatsView } from '../views/stats/stats';
import { BrowserView } from '../views/browser-view/browser-view';

/**
 * Flat routes rendered inside `AppShell` (WP-SHELL / F1-11). Each route
 * carries `data.screen`, which `ShellStore.activeScreen` derives from - the
 * sidebar/topbar highlight follows the router instead of a parallel signal.
 */
export const routes: Routes = [
  { path: '', component: StartView, data: { screen: 'start' } },
  // `/connect` merged into Start (F2-1); kept as an alias for old deep links.
  { path: 'connect', redirectTo: '', pathMatch: 'full' },
  { path: 'chat/:sessionID', component: ChatView, data: { screen: 'chat' } },
  { path: 'settings', component: SettingsView, data: { screen: 'settings' } },
  { path: 'terminal', component: TerminalView, data: { screen: 'terminal' } },
  { path: 'explorer', component: ExplorerView, data: { screen: 'explorer' } },
  { path: 'debug', component: DebugView, data: { screen: 'debug' } },
  { path: 'stats', component: StatsView, data: { screen: 'stats' } },
  // WP-BROWSER2 (F7-6): the browser viewer window. `bare: true` makes `App`
  // render it without the shell (own window, no sidebar/topbar/drawer).
  {
    path: 'browser-view',
    component: BrowserView,
    data: { screen: 'browserView', bare: true },
  },
  // The standalone Config page is retired; its replacement is the Settings
  // "Raw JSON" tab (built by WP-SETTINGS). Keep the old link working.
  { path: 'config', redirectTo: 'settings', pathMatch: 'full' },
  { path: '**', redirectTo: '' },
];
