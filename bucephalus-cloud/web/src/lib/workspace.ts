import { useEffect, useState } from "react"

export type WorkspacePreferences = {
  name: string
  slug: string
  defaultRegion: string
  idleMinutes: number
  apiBase: string
  userToken: string
}

const STORAGE_EVENT = "buc.workspace.changed"

export function readWorkspacePreferences(): WorkspacePreferences {
  return {
    name: localStorage.getItem("buc.workspaceName") || "Bucephalus Cloud",
    slug: localStorage.getItem("buc.workspaceSlug") || "workspace",
    defaultRegion: localStorage.getItem("buc.defaultRegion") || "us-east-1",
    idleMinutes: numberFromStorage("buc.idleMinutes", 20),
    apiBase: localStorage.getItem("buc.apiBase") || "",
    userToken: localStorage.getItem("buc.userToken") || "",
  }
}

export function writeWorkspacePreferences(next: WorkspacePreferences) {
  setOrRemove("buc.workspaceName", next.name.trim())
  setOrRemove("buc.workspaceSlug", next.slug.trim())
  setOrRemove("buc.defaultRegion", next.defaultRegion.trim())
  setOrRemove("buc.idleMinutes", String(next.idleMinutes || 20))
  setOrRemove("buc.apiBase", next.apiBase.trim())
  setOrRemove("buc.userToken", next.userToken.trim())
  window.dispatchEvent(new Event(STORAGE_EVENT))
}

export function useWorkspacePreferences() {
  const [prefs, setPrefs] = useState<WorkspacePreferences>(() => readWorkspacePreferences())

  useEffect(() => {
    const sync = () => setPrefs(readWorkspacePreferences())
    window.addEventListener("storage", sync)
    window.addEventListener(STORAGE_EVENT, sync)
    return () => {
      window.removeEventListener("storage", sync)
      window.removeEventListener(STORAGE_EVENT, sync)
    }
  }, [])

  return prefs
}

function setOrRemove(key: string, value: string) {
  if (value) localStorage.setItem(key, value)
  else localStorage.removeItem(key)
}

function numberFromStorage(key: string, fallback: number) {
  const parsed = Number(localStorage.getItem(key))
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback
}
