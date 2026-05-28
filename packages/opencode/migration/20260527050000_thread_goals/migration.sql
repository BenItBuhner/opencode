CREATE TABLE `thread_goal` (
  `session_id` text PRIMARY KEY NOT NULL,
  `goal_id` text NOT NULL,
  `objective` text NOT NULL,
  `status` text NOT NULL CHECK(`status` IN (
    'active',
    'paused',
    'blocked',
    'usage_limited',
    'budget_limited',
    'complete'
  )),
  `token_budget` integer,
  `tokens_used` integer DEFAULT 0 NOT NULL,
  `time_used_seconds` integer DEFAULT 0 NOT NULL,
  `created_at_ms` integer NOT NULL,
  `updated_at_ms` integer NOT NULL,
  FOREIGN KEY (`session_id`) REFERENCES `session`(`id`) ON UPDATE no action ON DELETE cascade
);
