import { Routes } from '@angular/router';

import { StartView } from '../views/start/start';
import { ChatView } from '../views/chat/chat';
import { SettingsView } from '../views/settings/settings';
import { TerminalView } from '../views/terminal/terminal';
import { ExplorerView } from '../views/explorer/explorer';
import { ConnectView } from '../views/connect/connect';
import { DebugView } from '../views/debug/debug';
import { ConfigView } from '../views/config/config';

export const routes: Routes = [
  { path: '', component: StartView },
  { path: 'connect', component: ConnectView },
  { path: 'chat/:sessionID', component: ChatView },
  { path: 'settings', component: SettingsView },
  { path: 'terminal', component: TerminalView },
  { path: 'explorer', component: ExplorerView },
  { path: 'debug', component: DebugView },
  { path: 'config', component: ConfigView },
  { path: '**', redirectTo: '' },
];
