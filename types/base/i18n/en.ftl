base-Person-name = { $firstName } { $lastName }
base-Person-description = Person
base-Company-name = { $name }
base-Company-description = Company
base-Group-name = Group { $name }
base-Group-description = Group of rights carriers
base-PersonAccount-name = Login { $login }
base-PersonAccount-description = Login of a person
base-AgentAccount-name = Agent { $login }
base-AgentAccount-description = Login of an agent
base-GroupMembership-name = Group membership
base-GroupMembership-description = Membership in a group
base-Room-name = Room { $name }
base-Room-description = Room
base-Task-name = { $subject }
base-Task-description = Task ({ $status })
base-Document-name = { $title }
base-Document-description = Document
base-Mail-name = { $sender } - { $subject }
    .fallback = { $sender } - { $subject } ({ $receivedAt })
base-Mail-description = Mail received { $receivedAt }
base-MailAttachment-name = { $filename }
base-MailAttachment-description = Mail attachment
base-mailbox--base-Mail-name = { $sender } - { $subject }
    .fallback = { $sender } - { $subject } ({ $receivedAt })
base-mailbox--base-Mail-description = Arrived { $receivedAt }
base-mailbox--label-sender = Sender
base-mailbox--label-subject = Subject
base-mailbox--label-received = Received
base-mailbox--label-attachmentCount = Attachments
base-mailbox--label-attachments = Attachments
base-mailbox--label-file = File
base-mailbox--label-added = Added
base-mailbox--label-reply = Reply

## Built-in views
base--label-_id = By id
base--label-_type = By type

## Aspect recipients
base-mailbox--label-_name = Mailbox
base-recipients--label-_name = Mails by recipient
base-recipients--base-Mail-name = { $sender } - { $subject }
    .fallback = { $sender } - { $subject } ({ $receivedAt })
base-recipients--label-attachments = Attachments
