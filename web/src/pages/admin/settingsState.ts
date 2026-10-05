import { makeAutoObservable } from 'mobx';

/** Whether the site settings form has unsaved edits (dot in the admin nav). */
class SettingsDirty {
  dirty = false;
  constructor() {
    makeAutoObservable(this);
  }
  set(v: boolean) {
    this.dirty = v;
  }
}

export const settingsDirty = new SettingsDirty();
