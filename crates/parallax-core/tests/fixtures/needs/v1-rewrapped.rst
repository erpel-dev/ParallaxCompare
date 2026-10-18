Requirements
===================

The login requirements of the demo
application.

.. req:: Login with password
   :id: REQ_001
   :status: open
   :tags: auth, security
   :priority: high

   The user can log in
   with a user name and a password.

   .. spec:: Password rules
      :id: SPEC_001
      :links: REQ_001

      At least 12 characters.

.. req:: Logout
   :id: REQ_002
   :status: open

   The user can log out.

.. impl:: Login form
   :id: IMPL_001
   :links: SPEC_001, REQ_001
   :status: done

   Implemented in ``login.py``.

.. test:: Login test
   :id: TEST_001
   :links: IMPL_001

   Checks the login.
