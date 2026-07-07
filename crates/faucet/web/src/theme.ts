const THEME_STORAGE_KEY = "theme"

const listeners = new Set<() => void>()

export type ThemePreference = "light" | "dark" | "system"
type ResolvedTheme = "light" | "dark"

function isThemePreference(value: string | null): value is ThemePreference {
  return value === "light" || value === "dark" || value === "system"
}

export function getThemePreference(): ThemePreference {
  const stored = window.localStorage.getItem(THEME_STORAGE_KEY)
  return isThemePreference(stored) ? stored : "system"
}

function resolveTheme(preference: ThemePreference): ResolvedTheme {
  if (preference === "system") {
    return window.matchMedia("(prefers-color-scheme: dark)").matches
      ? "dark"
      : "light"
  }
  return preference
}

function applyTheme(theme: ResolvedTheme) {
  const root = document.documentElement
  root.classList.toggle("dark", theme === "dark")
  root.style.colorScheme = theme
}

function updateTheme() {
  applyTheme(resolveTheme(getThemePreference()))
  for (const listener of listeners) {
    listener()
  }
}

export function setThemePreference(preference: ThemePreference) {
  window.localStorage.setItem(THEME_STORAGE_KEY, preference)
  updateTheme()
}

export function subscribeToThemePreference(listener: () => void) {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function syncThemeWithSystem() {
  const media = window.matchMedia("(prefers-color-scheme: dark)")
  const handleMediaChange = () => {
    if (getThemePreference() === "system") {
      updateTheme()
    }
  }
  const handleStorageChange = (event: StorageEvent) => {
    if (event.key === null || event.key === THEME_STORAGE_KEY) {
      updateTheme()
    }
  }

  updateTheme()
  media.addEventListener("change", handleMediaChange)
  window.addEventListener("storage", handleStorageChange)

  return () => {
    media.removeEventListener("change", handleMediaChange)
    window.removeEventListener("storage", handleStorageChange)
  }
}
