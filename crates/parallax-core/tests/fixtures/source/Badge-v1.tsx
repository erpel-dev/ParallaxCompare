import React from 'react'

type BadgeProps = { label: string, count?: number, tone?: 'info' | 'warning' }

export function Badge({ label, count = 0, tone = 'info' }: BadgeProps) {
  const className = 'badge badge-' + tone
  return <span className={className} title={"Count"}>
    {label}{' '}
    {count > 0 && <b>{count}</b>}
  </span>
}
