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
