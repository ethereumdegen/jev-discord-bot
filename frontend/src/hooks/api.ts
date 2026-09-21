import { createContext, useContext, useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient, type UseQueryOptions } from '@tanstack/react-query'
import { api, send } from '../api/client'
import type { Me } from '../api/types'

/** A GET, cached under its path. */
export function useApi<T>(path: string | null, options: { refetchInterval?: UseQueryOptions<T>['refetchInterval'] } = {}) {
  return useQuery<T>({ queryKey: [path], queryFn: () => api<T>(path as string), enabled: path !== null, refetchInterval: options.refetchInterval })
}

export function useMe() {
  const me = useApi<Me>('/me')
  return { ...me, account: me.data?.account ?? null, site: me.data?.site }
}

/** Where the sign-in lives: degenbuilders.com, through the hand-off. */
export const signInUrl = (returnTo: string) => `/api/auth/sso/start?return_to=${encodeURIComponent(returnTo)}`

/** Asked silently already, in this tab. */
const ASKED = 'dg_sso_asked'
/** Signed out here, deliberately: sticks until the next deliberate sign-in. */
const SIGNED_OUT = 'dg_signed_out'

/** Signing out has to stick, and a tab is not where that belongs: a new tab
 *  would silently sign the same browser straight back in. */
export const stopSigningInSilently = () => {
  localStorage.setItem(SIGNED_OUT, '1')
  sessionStorage.setItem(ASKED, '1')
}

/** Could this page still be about to hand off? Read before the first paint, so
 *  a sign-in screen never flashes in front of a redirect. */
const handoffPossible = () =>
  !localStorage.getItem(SIGNED_OUT) && !sessionStorage.getItem(ASKED) && new URL(window.location.href).searchParams.get('sso') !== 'none'

/**
 * Signed in at degenbuilders.com? Then you're signed in here. Once per tab we
 * ask silently; if nobody is signed in there we come back marked `sso=none`
 * and leave it alone until the next tab. Signing out here stops the asking
 * for good, until someone signs in again. Returns whether a hand-off is
 * coming, so the page can wait rather than offer a sign-in it's about to skip.
 */
export function useSingleSignOn() {
  const { account, site, isLoading } = useMe()
  const [handingOff, setHandingOff] = useState(handoffPossible)
  useEffect(() => {
    if (isLoading) return
    if (account) {
      // Signed in again: silent hand-offs are welcome from here on.
      localStorage.removeItem(SIGNED_OUT)
    }
    if (account || !site?.sso) {
      setHandingOff(false)
      return
    }
    const url = new URL(window.location.href)
    if (url.searchParams.get('sso') === 'none') {
      // Back from a silent ask: nobody is signed in over there. It was asked,
      // but nobody signed out — only the tab's mark belongs here.
      sessionStorage.setItem(ASKED, '1')
      window.history.replaceState(null, '', url.pathname + url.search.replace(/[?&]sso=none/, '').replace(/^&/, '?') + url.hash)
      setHandingOff(false)
      return
    }
    if (sessionStorage.getItem(ASKED) || localStorage.getItem(SIGNED_OUT)) {
      setHandingOff(false)
      return
    }
    sessionStorage.setItem(ASKED, '1')
    setHandingOff(true)
    window.location.replace(`${signInUrl(url.pathname + url.search)}&silent=1`)
  }, [account, site?.sso, isLoading])
  return handingOff
}

/** True while the browser is on its way to degenbuilders.com and back. */
export const HandingOff = createContext(false)
export const useHandingOff = () => useContext(HandingOff)

/** A write; everything is refetched after it succeeds (the pages are small). */
export function useAction<TInput = void, TResult = unknown>(method: string, path: string | ((input: TInput) => string), options: { body?: (input: TInput) => unknown; onSuccess?: (result: TResult) => void } = {}) {
  const client = useQueryClient()
  return useMutation<TResult, Error, TInput>({
    mutationFn: (input: TInput) => send<TResult>(method, typeof path === 'function' ? path(input) : path, options.body ? options.body(input) : input === undefined ? undefined : input),
    onSuccess: async (result) => {
      await client.invalidateQueries()
      options.onSuccess?.(result)
    },
  })
}
