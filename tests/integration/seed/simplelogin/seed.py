"""One invented SimpleLogin user for the alias suite, with two mailboxes and a fixed API key."""

from app.db import Session
from app.models import ApiKey, Mailbox, User
from server import create_light_app

with create_light_app().app_context():
    user = User.get_by(email="owner@example.org")
    if not user:
        user = User.create(
            email="owner@example.org",
            name="Owner",
            password="it-sl-user-password",
            activated=True,
            lifetime=True,
        )
        Session.flush()
        Mailbox.create(user_id=user.id, email="other@example.org", verified=True)
        key = ApiKey.create(user_id=user.id, name="holtmail-it")
        Session.flush()
        key.code = "it-sl-api-key"
        Session.commit()
