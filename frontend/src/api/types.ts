export interface Me {
  authenticated: boolean
  site: { brand: string; sso: boolean; sso_label: string | null; sso_url: string | null }
  account?: {
    id: string
    name: string
    email: string | null
    avatar_url: string | null
    discord_username: string | null
    discord_connected: boolean
    is_operator: boolean
  }
}

export interface ServerCard {
  id: string
  name: string
  icon: string | null
  owner: boolean
  installed: boolean
  mode: string | null
}

export type Step = 'none' | 'warn' | 'timeout' | 'kick' | 'ban'

export interface Server {
  id: string
  name: string
  icon: string | null
  mode: 'watch' | 'enforce' | 'paused'
  log_channel_id: string | null
  exempt_role_ids: string[]
  exempt_channel_ids: string[]
  confident_percent: number
  flag_percent: number
  strike_days: number
  timeout_minutes: number
  community: string
  monthly_allowance: number
  removed_at: string | null
}

export interface Rule {
  id: string
  kind: 'spam' | 'custom'
  name: string
  enabled: boolean
  ladder: Step[]
}

export interface ServerPage {
  server: Server
  rules: Rule[]
  month: {
    judged: number
    allowance: number
    actions: { review: number; strikes: number; warns: number; kicks: number; bans: number; undone: number; failed: number }
  }
}

export interface Action {
  id: string
  author_id: string
  username: string
  channel_id: string
  channel_name: string | null
  message_id: string
  excerpt: string
  verdict: string
  probabilities: Record<string, number>
  confidence: number
  lure: number
  outcome: 'review' | Step
  strike_number: number | null
  enforced: boolean
  message_deleted: boolean
  error: string | null
  created_at: string
  reversed_at: string | null
  reversed_by: string | null
  marked_wrong: boolean
}

export interface Named {
  id: string
  name: string
}
