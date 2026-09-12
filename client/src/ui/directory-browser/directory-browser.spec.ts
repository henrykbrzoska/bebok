import { pathCrumbs } from './directory-browser';

describe('pathCrumbs', () => {
  it('preserves a Windows drive root and its children', () => {
    expect(pathCrumbs('C:\\work\\bebok')).toEqual([
      { label: 'C:', path: 'C:\\' },
      { label: 'work', path: 'C:\\work' },
      { label: 'bebok', path: 'C:\\work\\bebok' },
    ]);
  });

  it('keeps the UNC server and share together as the navigable root', () => {
    expect(pathCrumbs('\\\\server\\share\\dir')).toEqual([
      { label: '\\\\server\\share', path: '\\\\server\\share' },
      { label: 'dir', path: '\\\\server\\share\\dir' },
    ]);
  });

  it('shows and navigates Unix root after a manually entered path', () => {
    expect(pathCrumbs('/home/rafal/project')).toEqual([
      { label: '/', path: '/' },
      { label: 'home', path: '/home' },
      { label: 'rafal', path: '/home/rafal' },
      { label: 'project', path: '/home/rafal/project' },
    ]);
  });
});
