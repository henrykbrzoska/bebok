import { Routes } from '@angular/router';

import { StartView } from '../views/start/start';
import { ChatView } from '../views/chat/chat';
import { SettingsView } from '../views/settings/settings';
import { TerminalView } from '../views/terminal/terminal';
import { ExplorerView } from '../views/explorer/explorer';
import { DebugView } from '../views/debug/debug';
import { StatsView } from '../views/stats/stats';
import { BrowserView } from '../views/browser-view/browser-view';
import { AboutView } from '../views/about/about';
import { UpdatesView } from '../views/updates/updates';
import { redirectToDesktop, redirectToMobile } from '../ui/mobile-shell/mobile.guards';

/**
 * Flat routes rendered inside `AppShell` (WP-SHELL / F1-11). Each route
 * carries `data.screen`, which `ShellStore.activeScreen` derives from - the
 * sidebar/topbar highlight follows the router instead of a parallel signal.
 *
 * WP-M2 (F10-8): the phone shell lives under the lazy `/m/**` group. Every
 * desktop route carries `redirectToMobile` (phone -> `/m` twin) and the `m`
 * group `redirectToDesktop` (desktop -> flat twin), see `mobile.guards.ts`.
 */
const mobile = [redirectToMobile];

export const routes: Routes = [
  { path: '', component: StartView, canActivate: mobile, data: { screen: 'start' } },
  // `/connect` merged into Start (F2-1); kept as an alias for old deep links.
  { path: 'connect', redirectTo: '', pathMatch: 'full' },
  { path: 'chat/:sessionID', component: ChatView, canActivate: mobile, data: { screen: 'chat' } },
  { path: 'settings', component: SettingsView, canActivate: mobile, data: { screen: 'settings' } },
  { path: 'terminal', component: TerminalView, canActivate: mobile, data: { screen: 'terminal' } },
  { path: 'explorer', component: ExplorerView, canActivate: mobile, data: { screen: 'explorer' } },
  { path: 'debug', component: DebugView, canActivate: mobile, data: { screen: 'debug' } },
  { path: 'stats', component: StatsView, canActivate: mobile, data: { screen: 'stats' } },
  // F8-4: static, localized "What is Bebok?" explainer page.
  { path: 'about', component: AboutView, canActivate: mobile, data: { screen: 'about' } },
  { path: 'updates', component: UpdatesView, canActivate: mobile, data: { screen: 'updates' } },
  // WP-M2 (F10-8): the mobile shell + its five tabs, one lazy chunk.
  {
    path: 'm',
    canMatch: [redirectToDesktop],
    loadChildren: () => import('../ui/mobile-shell/mobile.routes').then((m) => m.MOBILE_ROUTES),
  },
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
