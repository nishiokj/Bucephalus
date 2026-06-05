import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react"
import { Button } from "@/components/ui/button"
import { setCloudAuthTokenProvider } from "@/lib/cloud-api"
import { cn } from "@/lib/utils"

type GoogleCredentialResponse = {
  credential?: string
  select_by?: string
}

type GoogleMomentNotification = {
  isNotDisplayed: () => boolean
  isSkippedMoment: () => boolean
  getNotDisplayedReason: () => string
  getSkippedReason: () => string
}

type GoogleButtonConfig = {
  theme?: "outline" | "filled_blue" | "filled_black"
  size?: "large" | "medium" | "small"
  type?: "standard" | "icon"
  text?: "signin_with" | "signup_with" | "continue_with" | "signin"
  shape?: "rectangular" | "pill" | "circle" | "square"
  logo_alignment?: "left" | "center"
  width?: number
}

declare global {
  interface Window {
    google?: {
      accounts: {
        id: {
          initialize: (options: {
            client_id: string
            callback: (response: GoogleCredentialResponse) => void
            auto_select?: boolean
            cancel_on_tap_outside?: boolean
          }) => void
          prompt: (listener?: (notification: GoogleMomentNotification) => void) => void
          renderButton: (parent: HTMLElement, options: GoogleButtonConfig) => void
          disableAutoSelect: () => void
          revoke: (hint: string, callback: () => void) => void
        }
      }
    }
  }
}

type AuthStatus = "loading" | "signed_out" | "signed_in"

export type AuthUser = {
  subject: string
  email: string
  name: string
  picture: string
  expiresAt: number
}

type AuthContextValue = {
  status: AuthStatus
  ready: boolean
  configured: boolean
  clientId: string
  idToken: string | null
  user: AuthUser | null
  error: string | null
  signOut: () => void
  prompt: () => void
}

const SESSION_KEY = "buc.googleIdToken"
const GOOGLE_SCRIPT_ID = "google-identity-services"

const AuthContext = createContext<AuthContextValue | null>(null)

export function AuthProvider({ children }: { children: ReactNode }) {
  const clientId = googleOAuthClientId()
  const [token, setToken] = useState<string | null>(() => readStoredToken())
  const [ready, setReady] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const tokenRef = useRef(token)

  useEffect(() => {
    tokenRef.current = token
  }, [token])

  useEffect(() => {
    setCloudAuthTokenProvider(() => tokenRef.current)
  }, [])

  const acceptCredential = useCallback((credential: string) => {
    const user = userFromToken(credential)
    if (!user) {
      setError("Google returned a token the console could not read.")
      return
    }
    sessionStorage.setItem(SESSION_KEY, credential)
    setToken(credential)
    setError(null)
  }, [])

  useEffect(() => {
    if (!clientId) {
      setReady(false)
      return
    }
    let cancelled = false
    setError(null)
    void loadGoogleIdentityScript()
      .then(() => {
        if (cancelled) return
        window.google?.accounts.id.initialize({
          client_id: clientId,
          callback: (response) => {
            if (response.credential) acceptCredential(response.credential)
            else setError("Google sign-in did not return a usable session.")
          },
          auto_select: false,
          cancel_on_tap_outside: true,
        })
        setReady(true)
      })
      .catch((cause) => {
        if (!cancelled) {
          setReady(false)
          setError(cause instanceof Error ? cause.message : String(cause))
        }
      })
    return () => {
      cancelled = true
    }
  }, [acceptCredential, clientId])

  const user = useMemo(() => (token ? userFromToken(token) : null), [token])

  useEffect(() => {
    if (!token || user) return
    sessionStorage.removeItem(SESSION_KEY)
    setToken(null)
  }, [token, user])

  useEffect(() => {
    if (!user) return
    const msUntilExpiry = user.expiresAt * 1000 - Date.now()
    if (msUntilExpiry <= 0) {
      sessionStorage.removeItem(SESSION_KEY)
      setToken(null)
      return
    }
    const timeout = window.setTimeout(() => {
      sessionStorage.removeItem(SESSION_KEY)
      setToken(null)
    }, Math.min(msUntilExpiry, 2_147_483_647))
    return () => window.clearTimeout(timeout)
  }, [user])

  const signOut = useCallback(() => {
    const hint = user?.email || user?.subject
    sessionStorage.removeItem(SESSION_KEY)
    setToken(null)
    window.google?.accounts.id.disableAutoSelect()
    if (hint) {
      try {
        window.google?.accounts.id.revoke(hint, () => {})
      } catch {
        // Revocation is best-effort; local session clearing is what gates API calls.
      }
    }
  }, [user])

  const prompt = useCallback(() => {
    if (!clientId || !ready) return
    window.google?.accounts.id.prompt((notification) => {
      if (notification.isNotDisplayed()) {
        setError(`Google sign-in was not displayed: ${notification.getNotDisplayedReason()}`)
      } else if (notification.isSkippedMoment()) {
        setError(`Google sign-in was skipped: ${notification.getSkippedReason()}`)
      }
    })
  }, [clientId, ready])

  const value = useMemo<AuthContextValue>(() => ({
    status: token && user ? "signed_in" : clientId && !ready ? "loading" : "signed_out",
    ready,
    configured: Boolean(clientId),
    clientId,
    idToken: token && user ? token : null,
    user,
    error,
    signOut,
    prompt,
  }), [clientId, error, ready, signOut, prompt, token, user])

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>
}

export function useAuth() {
  const value = useContext(AuthContext)
  if (!value) {
    throw new Error("useAuth must be used inside AuthProvider")
  }
  return value
}

export function GoogleSignInButton({ className }: { className?: string }) {
  const { ready, configured } = useAuth()
  const hostRef = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    if (!ready || !configured || !hostRef.current || !window.google) return
    const host = hostRef.current
    host.replaceChildren()
    window.google.accounts.id.renderButton(host, {
      theme: "filled_black",
      size: "large",
      type: "standard",
      text: "signin_with",
      shape: "rectangular",
      logo_alignment: "left",
      width: Math.min(Math.max(host.clientWidth || 280, 220), 360),
    })
  }, [configured, ready])

  return (
    <div className={cn("min-h-10 w-full", className)}>
      <div ref={hostRef} className="w-full" />
      {configured && !ready ? (
        <Button disabled className="h-10 w-full bg-brand text-brand-foreground">
          Loading Google sign-in
        </Button>
      ) : null}
      {!configured ? (
        <Button disabled className="h-10 w-full bg-brand text-brand-foreground">
          Google OAuth client not configured
        </Button>
      ) : null}
    </div>
  )
}

function googleOAuthClientId() {
  return (
    window.BUCEPHALUS_WEB_CONFIG?.googleOAuthClientId
    || import.meta.env.VITE_BUCEPHALUS_GOOGLE_OAUTH_CLIENT_ID
    || import.meta.env.VITE_GOOGLE_OAUTH_CLIENT_ID
    || ""
  ).trim()
}

function readStoredToken() {
  const token = sessionStorage.getItem(SESSION_KEY)
  return token && userFromToken(token) ? token : null
}

function loadGoogleIdentityScript(): Promise<void> {
  if (window.google?.accounts.id) return Promise.resolve()
  const existing = document.getElementById(GOOGLE_SCRIPT_ID) as HTMLScriptElement | null
  if (existing) {
    return new Promise((resolve, reject) => {
      existing.addEventListener("load", () => resolve(), { once: true })
      existing.addEventListener("error", () => reject(new Error("Google Identity Services failed to load.")), { once: true })
    })
  }
  return new Promise((resolve, reject) => {
    const script = document.createElement("script")
    script.id = GOOGLE_SCRIPT_ID
    script.src = "https://accounts.google.com/gsi/client"
    script.async = true
    script.defer = true
    script.addEventListener("load", () => resolve(), { once: true })
    script.addEventListener("error", () => reject(new Error("Google Identity Services failed to load.")), { once: true })
    document.head.appendChild(script)
  })
}

function userFromToken(token: string): AuthUser | null {
  const payload = decodeJwtPayload(token)
  if (!payload) return null
  const expiresAt = numberClaim(payload.exp)
  if (!expiresAt || expiresAt * 1000 <= Date.now()) return null
  const subject = stringClaim(payload.sub)
  if (!subject) return null
  return {
    subject,
    email: stringClaim(payload.email),
    name: stringClaim(payload.name) || stringClaim(payload.email) || "Google user",
    picture: stringClaim(payload.picture),
    expiresAt,
  }
}

function decodeJwtPayload(token: string): Record<string, unknown> | null {
  const [, payload] = token.split(".")
  if (!payload) return null
  try {
    const normalized = payload.replaceAll("-", "+").replaceAll("_", "/")
    const padded = normalized.padEnd(normalized.length + ((4 - (normalized.length % 4)) % 4), "=")
    const json = decodeURIComponent(
      Array.from(atob(padded), (char) => `%${char.charCodeAt(0).toString(16).padStart(2, "0")}`).join(""),
    )
    const value = JSON.parse(json)
    return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : null
  } catch {
    return null
  }
}

function stringClaim(value: unknown) {
  return typeof value === "string" ? value : ""
}

function numberClaim(value: unknown) {
  return typeof value === "number" && Number.isFinite(value) ? value : 0
}
